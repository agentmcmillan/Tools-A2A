/// Onboarding state machine for peer projects.
///
/// When a user adds a project from a peer gateway, onboarding runs interactively:
///
///   ┌──────────────────────────────────────────────────────────────────┐
///   │  STEP 1 — awaiting_folder                                        │
///   │  Notify user (web UI + push notification):                        │
///   │    "Where should we place the project folder$1"                    │
///   │    Default: data/projects/{name}/                                 │
///   │                                                                   │
///   │  STEP 2 — awaiting_files                                          │
///   │  Notify user:                                                     │
///   │    "Which files should be included$1 (glob pattern)"               │
///   │    Default: **/*                                                  │
///   │                                                                   │
///   │  STEP 3 — building_context                                        │
///   │  Dispatch to user's own configured agent via LocalGateway.Call    │
///   │  Agent reads the task log + snapshot, builds a context summary    │
///   │                                                                   │
///   │  STEP 4 — awaiting_user_input                                     │
///   │  Notify user:                                                     │
///   │    "Context ready — here's what the project is about.             │
///   │     What do you want to work on or contribute$1"                   │
///   │                                                                   │
///   │  STEP 5 — done (onboarding_step = NULL)                           │
///   └──────────────────────────────────────────────────────────────────┘
///
/// "Notify user" means TWO channels simultaneously:
///   1. An in-memory `pending_prompt` that the web UI polls via htmx
///   2. A POST to the Entropy Reader push endpoint (→ iPhone notification)

use crate::context_builder::ContextBuilder;
use crate::db::Db;
use crate::projects::ProjectStore;
use crate::task_log::{EntryType, TaskLog};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::util::now_secs;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardingPrompt {
    pub project_id:     String,
    pub project_name:   String,
    pub step:           String,
    pub message:        String,
    pub default_value:  String,
    pub created_at:     f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardingAnswer {
    pub project_id: String,
    pub step:       String,
    pub value:      String,
}

// ── OnboardingManager ─────────────────────────────────────────────────────────

/// Manages interactive onboarding for peer projects.
///
/// Pending prompts are stored in memory and served to the web UI.
/// Push notifications are sent via the Entropy Reader webhook.
pub struct OnboardingManager {
    projects:         ProjectStore,
    task_log:         TaskLog,
    gateway_name:     String,
    entropy_webhook:  Option<String>,     // e.g. "http://192.168.1.67:8080/api/v1/claude-code/events"
    entropy_token:    Option<String>,     // Bearer token for Entropy Reader
    http_client:      reqwest::Client,
    /// Optional context builder — set via with_context_builder() after construction.
    /// When set, handle_files_answer() dispatches to the user's agent to analyse the project.
    context_builder:  Option<Arc<ContextBuilder>>,
    /// In-memory pending prompts for the web UI: project_id → prompt
    pending:          Arc<RwLock<HashMap<String, OnboardingPrompt>>>,
}

impl OnboardingManager {
    pub fn new(
        db:              Db,
        gateway_name:    impl Into<String>,
        jwt_secret:      impl AsRef<[u8]>,
        entropy_webhook: Option<String>,
        entropy_token:   Option<String>,
    ) -> Self {
        Self {
            projects:         ProjectStore::new(db.clone()),
            task_log:         TaskLog::new(db, jwt_secret),
            gateway_name:     gateway_name.into(),
            entropy_webhook,
            entropy_token,
            http_client:      reqwest::Client::new(),
            context_builder:  None,
            pending:          Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Attach a ContextBuilder so that `handle_files_answer` can dispatch to
    /// the user's agent once the glob is confirmed.
    pub fn with_context_builder(mut self, cb: ContextBuilder) -> Self {
        self.context_builder = Some(Arc::new(cb));
        self
    }

    // ── Step entry points ─────────────────────────────────────────────────

    /// Called after a peer project is added. Emits the first prompt.
    pub async fn start(&self, project_id: &str) -> Result<()> {
        let project = self.projects.get(project_id).await?
            .context("project not found")?;

        let default_path = format!("data/projects/{}", project.name);
        self.emit_prompt(OnboardingPrompt {
            project_id:    project.id.clone(),
            project_name:  project.name.clone(),
            step:          "awaiting_folder".into(),
            message:       format!(
                "New peer project '{}' added from {}. Where should the project folder be placed on this gateway$1",
                project.name, project.peer_gateway
            ),
            default_value: default_path,
            created_at:    now_secs(),
        }).await
    }

    /// Answer handler — called when the user submits a response via web UI.
    pub async fn answer(&self, ans: OnboardingAnswer) -> Result<()> {
        match ans.step.as_str() {
            "awaiting_folder" => self.handle_folder_answer(&ans.project_id, &ans.value).await,
            "awaiting_files"  => self.handle_files_answer(&ans.project_id, &ans.value).await,
            "awaiting_user_input" => self.handle_user_task(&ans.project_id, &ans.value).await,
            other => {
                tracing::warn!(step=other, project_id=%ans.project_id, "unexpected onboarding answer step");
                Ok(())
            }
        }
    }

    /// Retrieve and clear the pending prompt for the web UI (for polling).
    pub async fn take_prompt(&self, project_id: &str) -> Option<OnboardingPrompt> {
        self.pending.write().await.remove(project_id)
    }

    /// Peek at the prompt without consuming (for htmx polling).
    pub async fn peek_prompt(&self, project_id: &str) -> Option<OnboardingPrompt> {
        self.pending.read().await.get(project_id).cloned()
    }

    /// All currently pending prompts (for the dashboard).
    pub async fn all_pending(&self) -> Vec<OnboardingPrompt> {
        self.pending.read().await.values().cloned().collect()
    }

    // ── Context building (called from agent handler) ───────────────────────

    /// Transition from building_context → awaiting_user_input.
    /// This is called by the agent's response handler after it finishes context analysis.
    pub async fn context_ready(&self, project_id: &str, summary: &str) -> Result<()> {
        self.projects.set_onboarding_step(project_id, Some("awaiting_user_input")).await?;

        let project = self.projects.get(project_id).await?
            .context("project not found")?;

        self.emit_prompt(OnboardingPrompt {
            project_id:   project_id.to_owned(),
            project_name: project.name.clone(),
            step:         "awaiting_user_input".into(),
            message:      format!(
                "Context analysis complete for '{}'.\n\n{}\n\nWhat would you like to work on or contribute$1",
                project.name, summary
            ),
            default_value: String::new(),
            created_at:   now_secs(),
        }).await
    }

    // ── Private ───────────────────────────────────────────────────────────

    async fn handle_folder_answer(&self, project_id: &str, folder_path: &str) -> Result<()> {
        let path = if folder_path.trim().is_empty() {
            let p = self.projects.get(project_id).await?.context("project not found")?;
            format!("data/projects/{}", p.name)
        } else {
            folder_path.trim().to_owned()
        };

        self.projects.set_folder_path(project_id, &path).await?;
        self.projects.set_onboarding_step(project_id, Some("awaiting_files")).await?;

        let project = self.projects.get(project_id).await?.context("project not found")?;
        self.emit_prompt(OnboardingPrompt {
            project_id:   project_id.to_owned(),
            project_name: project.name.clone(),
            step:         "awaiting_files".into(),
            message:      format!(
                "Project folder set to '{path}'. \
                 Which files should be included in the sync$1 \
                 Use a glob pattern (e.g. **/*.rs, **/*.md, **/*)"
            ),
            default_value: "**/*".into(),
            created_at:   now_secs(),
        }).await
    }

    async fn handle_files_answer(&self, project_id: &str, glob: &str) -> Result<()> {
        let glob = if glob.trim().is_empty() { "**/*" } else { glob.trim() };

        self.projects.set_file_glob(project_id, glob).await?;
        self.projects.set_onboarding_step(project_id, Some("building_context")).await?;

        // Log the context-building task for the user's own agent
        let project = self.projects.get(project_id).await?.context("project not found")?;
        self.task_log.append(
            project_id,
            &self.gateway_name,
            "gateway",
            EntryType::Note,
            &format!("Onboarding: building context for '{}' with glob '{glob}'", project.name),
            None,
        ).await?;

        // Fire-and-forget: dispatch to the user's configured agent so it can
        // analyse the task log and call back via context_ready() when done.
        if let Some(ref cb) = self.context_builder {
            let cb       = cb.clone();
            let pid      = project_id.to_owned();
            let pname    = project.name.clone();
            tokio::spawn(async move {
                match cb.dispatch(&pid, &pname).await {
                    Ok(agent) => tracing::info!(agent=%agent, project=%pname, "context-build dispatched"),
                    Err(e)    => tracing::warn!(error=%e, project=%pname, "context-build dispatch failed"),
                }
            });
        }

        self.push_notification(
            &format!("Building context for '{}' — your agent is analysing the project now.", project.name),
            "onboarding",
        ).await;

        Ok(())
    }

    async fn handle_user_task(&self, project_id: &str, task: &str) -> Result<()> {
        if task.trim().is_empty() {
            return Ok(());
        }

        // Write the user's stated task intention to the task log
        let project = self.projects.get(project_id).await?.context("project not found")?;
        self.task_log.append(
            project_id,
            &self.gateway_name,
            "user",
            EntryType::Focus,
            task,
            None,
        ).await?;

        // Onboarding complete
        self.projects.set_onboarding_step(project_id, None).await?;
        self.clear_prompt(project_id).await;

        self.push_notification(
            &format!("Onboarding complete for '{}'. Task logged: {task}", project.name),
            "onboarding",
        ).await;

        Ok(())
    }

    async fn emit_prompt(&self, prompt: OnboardingPrompt) -> Result<()> {
        // 1. Store in memory for web UI polling
        self.pending.write().await.insert(prompt.project_id.clone(), prompt.clone());

        // 2. Push to Entropy Reader (fire-and-forget; don't fail onboarding if push fails)
        self.push_notification(&prompt.message, &prompt.step).await;

        Ok(())
    }

    async fn clear_prompt(&self, project_id: &str) {
        self.pending.write().await.remove(project_id);
    }

    async fn push_notification(&self, message: &str, event_type: &str) {
        let Some(webhook) = &self.entropy_webhook else { return; };

        let body = serde_json::json!({
            "event":   event_type,
            "message": message,
            "source":  "a2a-gateway",
            "gateway": self.gateway_name,
        });

        let mut req = self.http_client.post(webhook).json(&body);
        if let Some(token) = &self.entropy_token {
            req = req.bearer_auth(token);
        }

        if let Err(e) = req.send().await {
            tracing::warn!(error=%e, "failed to send Entropy Reader notification");
        }
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn mgr() -> (OnboardingManager, Db) {
        let db = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let m  = OnboardingManager::new(db.clone(), "site-a", b"secret", None, None);
        (m, db)
    }

    async fn insert_peer_project(db: &Db) -> String {
        let id  = uuid::Uuid::new_v4().to_string();
        let now = now_secs();
        sqlx::query(
            "INSERT INTO projects \
             (id,name,repo_url,description,visibility,folder_path,file_glob,\
              is_active,created_at,updated_at,onboarding_step,origin_gateway,peer_gateway,peer_project_id) \
             VALUES ($1,$2,'https://x.com','','lan-only','','**/*',0,$3,$4,'awaiting_folder','site-a','site-b','r1')"
        )
        .bind(&id).bind("myproject").bind(now).bind(now)
        .execute(db).await.unwrap();
        id
    }

    #[tokio::test]
    async fn start_emits_folder_prompt() {
        let (mgr, db) = mgr().await;
        let pid = insert_peer_project(&db).await;
        mgr.start(&pid).await.unwrap();

        let prompt = mgr.peek_prompt(&pid).await.unwrap();
        assert_eq!(prompt.step, "awaiting_folder");
        assert!(prompt.message.contains("myproject"));
        assert!(prompt.default_value.contains("myproject"));
    }

    #[tokio::test]
    async fn folder_answer_advances_to_files() {
        let (mgr, db) = mgr().await;
        let pid = insert_peer_project(&db).await;
        mgr.start(&pid).await.unwrap();

        mgr.answer(OnboardingAnswer {
            project_id: pid.clone(),
            step:       "awaiting_folder".into(),
            value:      "/tmp/myproject".into(),
        }).await.unwrap();

        let prompt = mgr.peek_prompt(&pid).await.unwrap();
        assert_eq!(prompt.step, "awaiting_files");

        let project = mgr.projects.get(&pid).await.unwrap().unwrap();
        assert_eq!(project.folder_path, "/tmp/myproject");
        assert_eq!(project.onboarding_step.as_deref(), Some("awaiting_files"));
    }

    #[tokio::test]
    async fn files_answer_advances_to_building_context() {
        let (mgr, db) = mgr().await;
        let pid = insert_peer_project(&db).await;
        mgr.start(&pid).await.unwrap();
        mgr.answer(OnboardingAnswer { project_id: pid.clone(), step: "awaiting_folder".into(), value: "/tmp/p".into() }).await.unwrap();
        mgr.answer(OnboardingAnswer { project_id: pid.clone(), step: "awaiting_files".into(), value: "**/*.rs".into() }).await.unwrap();

        let project = mgr.projects.get(&pid).await.unwrap().unwrap();
        assert_eq!(project.file_glob, "**/*.rs");
        assert_eq!(project.onboarding_step.as_deref(), Some("building_context"));
    }

    #[tokio::test]
    async fn context_ready_emits_user_input_prompt() {
        let (mgr, db) = mgr().await;
        let pid = insert_peer_project(&db).await;
        mgr.projects.set_onboarding_step(&pid, Some("building_context")).await.unwrap();
        mgr.context_ready(&pid, "This project is a Rust A2A gateway.").await.unwrap();

        let prompt = mgr.peek_prompt(&pid).await.unwrap();
        assert_eq!(prompt.step, "awaiting_user_input");
        assert!(prompt.message.contains("Rust A2A gateway"));
    }

    #[tokio::test]
    async fn user_task_completes_onboarding() {
        let (mgr, db) = mgr().await;
        let pid = insert_peer_project(&db).await;
        mgr.projects.set_onboarding_step(&pid, Some("awaiting_user_input")).await.unwrap();
        // Put something in pending so we can verify it gets cleared
        mgr.pending.write().await.insert(pid.clone(), OnboardingPrompt {
            project_id: pid.clone(), project_name: "test".into(),
            step: "awaiting_user_input".into(), message: "m".into(),
            default_value: "".into(), created_at: 0.0,
        });

        mgr.answer(OnboardingAnswer {
            project_id: pid.clone(),
            step:       "awaiting_user_input".into(),
            value:      "I want to add authentication".into(),
        }).await.unwrap();

        let project = mgr.projects.get(&pid).await.unwrap().unwrap();
        assert!(project.onboarding_step.is_none(), "onboarding should be done");
        assert!(mgr.peek_prompt(&pid).await.is_none(), "prompt should be cleared");
    }
}

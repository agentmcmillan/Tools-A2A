/// Context builder — dispatches a "build_context" call to the user's own
/// configured agent via the LocalGateway gRPC endpoint.
///
/// This is the bridge between Phase 2 onboarding and the user's AI agent.
/// When a peer project reaches the `building_context` onboarding step, the
/// gateway calls the user's agent with a summary of the task log + snapshot,
/// and expects a natural-language summary back.
///
/// Flow:
///   OnboardingManager::handle_files_answer()
///     → ContextBuilder::dispatch()
///       → LocalGateway.Call(target_agent, { "action": "build_context", ... })
///         → agent processes and calls back LocalGateway.Call("gateway", { result })
///           → OnboardingManager::context_ready()
///
/// The call is fire-and-forget from the gateway's perspective; the round-trip
/// happens asynchronously. The agent calls back via the existing Call RPC.
///
/// The agent to call is the first registered agent capable of "context_building"
/// (checked against capabilities), or the first agent alphabetically if none
/// declare that capability.

use crate::db::Db;
use crate::registry::Registry;
use crate::task_log::TaskLog;
use anyhow::{bail, Context, Result};
use serde_json::json;
use tracing::info;

// ── ContextBuilder ────────────────────────────────────────────────────────────

pub struct ContextBuilder {
    registry:     Registry,
    task_log:     TaskLog,
    gateway_name: String,
    http_client:  reqwest::Client,
}

impl ContextBuilder {
    pub fn new(
        db:           Db,
        registry:     Registry,
        gateway_name: impl Into<String>,
        jwt_secret:   impl AsRef<[u8]>,
    ) -> Self {
        Self {
            registry,
            task_log:     TaskLog::new(db, jwt_secret),
            gateway_name: gateway_name.into(),
            http_client:  reqwest::Client::new(),
        }
    }

    /// Dispatch a context-building request to the user's own agent.
    ///
    /// Selects the best available agent, sends the project task log and metadata
    /// via its /invoke HTTP endpoint, and returns immediately.  The agent is
    /// expected to call back via the gateway to complete onboarding.
    ///
    /// Returns the name of the agent that was dispatched to.
    pub async fn dispatch(
        &self,
        project_id:   &str,
        project_name: &str,
    ) -> Result<String> {
        let agent = self.select_agent()
            .context("no agents registered — cannot build context")?;

        // Pull the last 50 task log entries for the project to give the agent context
        let entries = self.task_log.since(project_id, 0).await?;
        let log_summary: Vec<serde_json::Value> = entries.iter().take(50).map(|e| json!({
            "cursor":     e.cursor,
            "gateway":    e.gateway_name,
            "agent":      e.agent_name,
            "type":       e.entry_type.as_str(),
            "content":    e.content,
            "created_at": e.created_at,
        })).collect();

        let payload = json!({
            "action":       "build_context",
            "project_id":   project_id,
            "project_name": project_name,
            "gateway":      self.gateway_name,
            "task_log":     log_summary,
            "instructions": concat!(
                "You are being asked to build a context summary for a peer project that has been ",
                "added to this gateway. Read the task log entries above and write a concise ",
                "1-3 paragraph summary describing: what the project is, what work has been done, ",
                "and what the current focus is. Respond with a JSON object: ",
                "{ \"summary\": \"<your summary here>\" }"
            ),
        });

        info!(agent=%agent.name, project=%project_name, "dispatching context-build call");

        let resp = self.http_client
            .post(format!("{}/invoke", agent.endpoint))
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("calling agent '{}' at {}", agent.name, agent.endpoint))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("agent '{}' returned {}: {}", agent.name, status, body);
        }

        info!(agent=%agent.name, project=%project_name, "context-build dispatched successfully");
        Ok(agent.name.clone())
    }

    /// Selects the most appropriate agent for context building.
    ///
    /// Preference order:
    ///   1. Agent with "context_building" in capabilities
    ///   2. Agent with "orchestrator" in capabilities
    ///   3. First registered agent (alphabetical by name)
    fn select_agent(&self) -> Option<crate::registry::AgentEntry> {
        let mut agents = self.registry.list();
        if agents.is_empty() {
            return None;
        }

        // Sort deterministically so we always pick the same agent in tests
        agents.sort_by(|a, b| a.name.cmp(&b.name));

        // Prefer an agent that explicitly declares context-building capability
        if let Some(a) = agents.iter().find(|a| {
            a.capabilities.iter().any(|c| c == "context_building")
        }) {
            return Some(a.clone());
        }

        // Fall back to orchestrator
        if let Some(a) = agents.iter().find(|a| {
            a.capabilities.iter().any(|c| c.contains("orchestrat"))
        }) {
            return Some(a.clone());
        }

        // Fallback: first agent
        agents.into_iter().next()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, registry::{AgentEntry, Registry}};
    use chrono::Utc;

    async fn make_builder(agents: &[(&str, &[&str])]) -> ContextBuilder {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let reg = Registry::new(db.clone()).await.unwrap();
        for (name, caps) in agents {
            reg.upsert(AgentEntry {
                name:          name.to_string(),
                version:       "1".into(),
                endpoint:      format!("http://localhost:9999/{name}"),
                capabilities:  caps.iter().map(|s| s.to_string()).collect(),
                soul_toml:     String::new(),
                last_seen:     None,
                registered_at: Utc::now().to_rfc3339(),
            }).await.unwrap();
        }
        ContextBuilder::new(db, reg, "site-a", b"secret")
    }

    #[tokio::test]
    async fn no_agents_returns_none() {
        let builder = make_builder(&[]).await;
        assert!(builder.select_agent().is_none());
    }

    #[tokio::test]
    async fn prefers_context_building_capability() {
        let builder = make_builder(&[
            ("alpha",  &["writing"]),
            ("bravo",  &["context_building", "research"]),
            ("charlie",&["orchestrator"]),
        ]).await;
        let selected = builder.select_agent().unwrap();
        assert_eq!(selected.name, "bravo");
    }

    #[tokio::test]
    async fn falls_back_to_orchestrator() {
        let builder = make_builder(&[
            ("alpha",  &["writing"]),
            ("bravo",  &["orchestrator"]),
        ]).await;
        let selected = builder.select_agent().unwrap();
        assert_eq!(selected.name, "bravo");
    }

    #[tokio::test]
    async fn falls_back_to_alphabetical_first() {
        let builder = make_builder(&[
            ("zeta",  &["writing"]),
            ("alpha", &["research"]),
        ]).await;
        let selected = builder.select_agent().unwrap();
        assert_eq!(selected.name, "alpha");
    }
}

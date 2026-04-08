/// Rules-based task reconciler.
///
/// Runs every N seconds (configurable via gateway.toml).
/// No LLM — pure rule evaluation:
///
/// ┌─────────────────────────────────────────────────────────────────┐
/// │  Every reconcile_interval_secs:                                 │
/// │  1. Load all open agent todos                                   │
/// │  2. Load unassigned project tasks from requests                 │
/// │  3. Match tasks → agents by capability overlap                  │
/// │  4. Detect & merge duplicate todos (same task text, two agents) │
/// │  5. Detect conflicts (contradictory assignments)                │
/// │  6. Write updated project_todos                                 │
/// │  7. Emit todo.md to disk                                        │
/// └─────────────────────────────────────────────────────────────────┘

use anyhow::Result;
use chrono::Utc;
use crate::db::Db;
use crate::nats_bus::Bus;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{info, warn};

pub struct Reconciler {
    db:       Db,
    interval: Duration,
    data_dir: String,
    bus:       Arc<Bus>,
    _gateway:  String,
}

impl Reconciler {
    pub fn new(
        db: Db,
        interval_secs: u64,
        data_dir: impl Into<String>,
        bus: Arc<Bus>,
        gateway: impl Into<String>,
    ) -> Self {
        Self {
            db,
            interval: Duration::from_secs(interval_secs),
            data_dir: data_dir.into(),
            bus,
            _gateway: gateway.into(),
        }
    }

    /// Spawn a background task that runs the reconcile loop.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = time::interval(self.interval);
            loop {
                ticker.tick().await;
                if let Err(e) = self.run_once().await {
                    warn!("reconciler error: {e:#}");
                }
            }
        })
    }

    /// Run a single reconciliation pass. Public so tests can call it directly.
    pub async fn run_once(&self) -> Result<()> {
        let started = Utc::now();
        let span = tracing::info_span!("reconcile", ts = %started.to_rfc3339());
        let _enter = span.enter();

        let (merged, assigned, flagged) = self.reconcile_todos().await?;
        self.write_todo_md().await?;

        info!(merged, assigned, flagged, "reconcile complete");
        Ok(())
    }

    // ── Core logic ────────────────────────────────────────────────────────────

    async fn reconcile_todos(&self) -> Result<(usize, usize, usize)> {
        // Load all open agent todos grouped by task text
        let rows = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT agent_name, task, id FROM todos WHERE status != 'done' ORDER BY task, id"
        )
        .fetch_all(&self.db)
        .await?;

        // Group by normalised task text
        let mut by_task: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        for (agent, task, id) in &rows {
            by_task
                .entry(normalise_task(task))
                .or_default()
                .push((agent.clone(), *id));
        }

        let mut merged   = 0usize;
        let mut assigned = 0usize;
        let mut flagged  = 0usize;
        let now = Utc::now().to_rfc3339();

        for (task_key, entries) in &by_task {
            if entries.len() > 1 {
                // Duplicate: merge into a single project_todo
                merged += 1;
                let owners: Vec<String> = entries.iter().map(|(a, _)| a.clone()).collect();
                let source_ids: Vec<i64> = entries.iter().map(|(_, id)| *id).collect();
                self.upsert_project_todo(
                    task_key,
                    Some(&owners[0]),      // assign to first owner
                    "pending",
                    &format!("merged from {} agents: {}", owners.len(), owners.join(", ")),
                    &source_ids,
                    &now,
                )
                .await?;
            } else {
                let (agent, _id) = &entries[0];
                // Check agent exists and task matches a capability
                if self.agent_has_capability(agent, task_key).await? {
                    self.upsert_project_todo(
                        task_key,
                        Some(agent),
                        "pending",
                        "",
                        &[entries[0].1],
                        &now,
                    )
                    .await?;
                    // Notify the agent via NATS hook
                    // project_id is not available in this code path (reconciler works
                    // across all projects). Pass task_key as identifier.
                    self.bus.publish_task_assigned(agent, "global", task_key, Some(entries[0].1)).await;
                    assigned += 1;
                } else {
                    // Flag: agent doesn't obviously have the capability
                    self.upsert_project_todo(
                        task_key,
                        Some(agent),
                        "flagged",
                        "capability mismatch — review required",
                        &[entries[0].1],
                        &now,
                    )
                    .await?;
                    flagged += 1;
                }
            }
        }

        Ok((merged, assigned, flagged))
    }

    async fn agent_has_capability(&self, agent: &str, task: &str) -> Result<bool> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT capabilities FROM agents WHERE name = $1")
                .bind(agent)
                .fetch_optional(&self.db)
                .await?;

        if let Some((caps_json,)) = row {
            let caps: Vec<String> = serde_json::from_str(&caps_json).unwrap_or_default();
            let task_lower = task.to_lowercase();
            return Ok(caps.iter().any(|c| task_lower.contains(c.to_lowercase().as_str())));
        }
        Ok(false)
    }

    async fn upsert_project_todo(
        &self,
        task:        &str,
        assigned_to: Option<&str>,
        status:      &str,
        notes:       &str,
        source_ids:  &[i64],
        now:         &str,
    ) -> Result<()> {
        let source_json = serde_json::to_string(source_ids)?;
        sqlx::query(
            r#"INSERT INTO project_todos (task, assigned_to, status, notes, source_todos, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $6)
               ON CONFLICT DO NOTHING"#
        )
        .bind(task)
        .bind(assigned_to)
        .bind(status)
        .bind(notes)
        .bind(&source_json)
        .bind(now)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn write_todo_md(&self) -> Result<()> {
        let rows = sqlx::query_as::<_, (String, Option<String>, String, String)>(
            "SELECT task, assigned_to, status, notes FROM project_todos ORDER BY id"
        )
        .fetch_all(&self.db)
        .await?;

        let mut md = String::from("# Project TODO\n\n_Generated by a2a-gateway reconciler._\n\n");
        for (task, agent, status, notes) in &rows {
            let icon = match status.as_str() {
                "done"    => "✅",
                "flagged" => "⚠️",
                _         => "⬜",
            };
            let owner = agent.as_deref().unwrap_or("unassigned");
            md.push_str(&format!("- {icon} **{task}**  `{owner}`\n"));
            if !notes.is_empty() {
                md.push_str(&format!("  _{notes}_\n"));
            }
        }

        let path = format!("{}/todo.md", self.data_dir);
        if let Err(e) = std::fs::write(&path, md) {
            warn!("could not write {path}: {e}");
        }
        Ok(())
    }
}

fn normalise_task(s: &str) -> String {
    s.trim().to_lowercase()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::registry::{AgentEntry, Registry};

    async fn setup(agents: &[(&str, &[&str])]) -> (Db, Reconciler) {
        let pool = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let reg = Registry::new(pool.clone()).await.unwrap();
        for (name, caps) in agents {
            reg.upsert(AgentEntry {
                name:          (*name).into(),
                version:       "0.1.0".into(),
                endpoint:      format!("http://{name}:8080"),
                capabilities:  caps.iter().map(|s| (*s).into()).collect(),
                soul_toml:     String::new(),
                last_seen:     None,
                registered_at: Utc::now().to_rfc3339(),
            }).await.unwrap();
        }
        let r = Reconciler::new(pool.clone(), 30, "/tmp", Arc::new(Bus::Null), "test-gw");
        (pool, r)
    }

    async fn add_todo(pool: &Db, agent: &str, task: &str) {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO todos (agent_name, task, status, created_at) VALUES ($1, $2, 'pending', $3)"
        )
        .bind(agent)
        .bind(task)
        .bind(&now)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn project_todos(pool: &Db) -> Vec<(String, Option<String>, String)> {
        sqlx::query_as::<_, (String, Option<String>, String)>(
            "SELECT task, assigned_to, status FROM project_todos ORDER BY id"
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn duplicate_todos_merged() {
        let (pool, rec) = setup(&[
            ("research", &["research"]),
            ("writer",   &["write"]),
        ]).await;
        add_todo(&pool, "research", "research topic X").await;
        add_todo(&pool, "writer",   "research topic X").await;

        rec.run_once().await.unwrap();

        let todos = project_todos(&pool).await;
        assert_eq!(todos.len(), 1);
        assert_eq!(normalise_task(&todos[0].0), "research topic x");
    }

    #[tokio::test]
    async fn single_todo_assigned_by_capability() {
        let (pool, rec) = setup(&[
            ("research", &["research", "search"]),
        ]).await;
        add_todo(&pool, "research", "search for papers").await;

        rec.run_once().await.unwrap();

        let todos = project_todos(&pool).await;
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].1.as_deref(), Some("research"));
        assert_eq!(todos[0].2, "pending");
    }

    #[tokio::test]
    async fn mismatched_capability_flagged() {
        let (pool, rec) = setup(&[
            ("writer", &["write", "draft"]),
        ]).await;
        add_todo(&pool, "writer", "search for papers").await; // 'search' not in writer caps

        rec.run_once().await.unwrap();

        let todos = project_todos(&pool).await;
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].2, "flagged");
    }
}

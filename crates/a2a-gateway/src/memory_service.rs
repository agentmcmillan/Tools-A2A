/// Verbatim memory storage: typed facts with filtered retrieval.
///
/// Facts are stored exactly as provided — never summarized, truncated, or merged.
/// Each fact has an optional project scope, topic tag, and validity window.
/// Expired facts (valid_to < now) are automatically excluded from recall queries.

use crate::db::Db;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id:         i64,
    pub agent_name: String,
    pub project_id: Option<String>,
    pub topic:      String,
    pub content:    String,
    pub created_at: f64,
    pub valid_from: f64,
    pub valid_to:   Option<f64>,
}

// ── MemoryService ────────────────────────────────────────────────────────────

pub struct MemoryService {
    db: Db,
}

impl MemoryService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Store a verbatim fact for an agent.
    ///
    /// `project_id` scopes the fact to a project (None = global).
    /// `topic` is a free-form tag for filtered retrieval.
    pub async fn store(
        &self,
        agent_name: &str,
        content:    &str,
        project_id: Option<&str>,
        topic:      &str,
    ) -> Result<MemoryFact> {
        let now = now_secs();
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO memory_facts \
             (agent_name, project_id, topic, content, created_at, valid_from) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             RETURNING id"
        )
        .bind(agent_name)
        .bind(project_id)
        .bind(topic)
        .bind(content)
        .bind(now)
        .bind(now)
        .fetch_one(&self.db)
        .await
        .context("insert memory fact")?;

        Ok(MemoryFact {
            id:         row.0,
            agent_name: agent_name.to_owned(),
            project_id: project_id.map(String::from),
            topic:      topic.to_owned(),
            content:    content.to_owned(),
            created_at: now,
            valid_from: now,
            valid_to:   None,
        })
    }

    /// Recall non-expired facts for an agent, filtered by optional project and topic.
    ///
    /// Results are ordered newest-first, capped at `limit`.
    pub async fn recall(
        &self,
        agent_name: &str,
        project_id: Option<&str>,
        topic:      Option<&str>,
        limit:      usize,
    ) -> Result<Vec<MemoryFact>> {
        let now = now_secs();

        // Build the query dynamically based on which filters are supplied.
        // All branches share the same base WHERE to exclude expired entries.
        let (sql, rows) = match (project_id, topic) {
            (Some(pid), Some(t)) => {
                let rows = sqlx::query_as::<_, MemoryFactRow>(
                    "SELECT * FROM memory_facts \
                     WHERE agent_name = $1 AND project_id = $2 AND topic = $3 \
                       AND (valid_to IS NULL OR valid_to > $4) \
                     ORDER BY created_at DESC \
                     LIMIT $5"
                )
                .bind(agent_name)
                .bind(pid)
                .bind(t)
                .bind(now)
                .bind(limit as i64)
                .fetch_all(&self.db)
                .await
                .context("recall(project+topic)")?;
                ("project+topic", rows)
            }
            (Some(pid), None) => {
                let rows = sqlx::query_as::<_, MemoryFactRow>(
                    "SELECT * FROM memory_facts \
                     WHERE agent_name = $1 AND project_id = $2 \
                       AND (valid_to IS NULL OR valid_to > $3) \
                     ORDER BY created_at DESC \
                     LIMIT $4"
                )
                .bind(agent_name)
                .bind(pid)
                .bind(now)
                .bind(limit as i64)
                .fetch_all(&self.db)
                .await
                .context("recall(project)")?;
                ("project", rows)
            }
            (None, Some(t)) => {
                let rows = sqlx::query_as::<_, MemoryFactRow>(
                    "SELECT * FROM memory_facts \
                     WHERE agent_name = $1 AND topic = $2 \
                       AND (valid_to IS NULL OR valid_to > $3) \
                     ORDER BY created_at DESC \
                     LIMIT $4"
                )
                .bind(agent_name)
                .bind(t)
                .bind(now)
                .bind(limit as i64)
                .fetch_all(&self.db)
                .await
                .context("recall(topic)")?;
                ("topic", rows)
            }
            (None, None) => {
                let rows = sqlx::query_as::<_, MemoryFactRow>(
                    "SELECT * FROM memory_facts \
                     WHERE agent_name = $1 \
                       AND (valid_to IS NULL OR valid_to > $2) \
                     ORDER BY created_at DESC \
                     LIMIT $3"
                )
                .bind(agent_name)
                .bind(now)
                .bind(limit as i64)
                .fetch_all(&self.db)
                .await
                .context("recall(all)")?;
                ("all", rows)
            }
        };
        let _ = sql; // used only for context strings above

        Ok(rows.into_iter().map(MemoryFact::from).collect())
    }

    /// All non-expired facts for an agent, regardless of project or topic.
    pub async fn recall_all(&self, agent_name: &str) -> Result<Vec<MemoryFact>> {
        let now = now_secs();
        let rows = sqlx::query_as::<_, MemoryFactRow>(
            "SELECT * FROM memory_facts \
             WHERE agent_name = $1 \
               AND (valid_to IS NULL OR valid_to > $2) \
             ORDER BY created_at DESC"
        )
        .bind(agent_name)
        .bind(now)
        .fetch_all(&self.db)
        .await
        .context("recall_all")?;
        Ok(rows.into_iter().map(MemoryFact::from).collect())
    }

    /// Expire a fact immediately by setting valid_to = now.
    pub async fn expire(&self, id: i64) -> Result<()> {
        let now = now_secs();
        sqlx::query("UPDATE memory_facts SET valid_to = $1 WHERE id = $2")
            .bind(now)
            .bind(id)
            .execute(&self.db)
            .await
            .context("expire memory fact")?;
        Ok(())
    }
}

// ── SQLx row mapping ─────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct MemoryFactRow {
    id:         i64,
    agent_name: String,
    project_id: Option<String>,
    topic:      String,
    content:    String,
    created_at: f64,
    valid_from: f64,
    valid_to:   Option<f64>,
}

impl From<MemoryFactRow> for MemoryFact {
    fn from(r: MemoryFactRow) -> Self {
        Self {
            id:         r.id,
            agent_name: r.agent_name,
            project_id: r.project_id,
            topic:      r.topic,
            content:    r.content,
            created_at: r.created_at,
            valid_from: r.valid_from,
            valid_to:   r.valid_to,
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::registry::{AgentEntry, Registry};
    use chrono::Utc;

    async fn setup() -> MemoryService {
        let pool = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let reg = Registry::new(pool.clone()).await.unwrap();
        reg.upsert(AgentEntry {
            name:          "mem-agent".into(),
            version:       "0.1.0".into(),
            endpoint:      "http://mem-agent:8080".into(),
            capabilities:  vec![],
            soul_toml:     String::new(),
            last_seen:     None,
            registered_at: Utc::now().to_rfc3339(),
        }).await.unwrap();
        MemoryService::new(pool)
    }

    #[tokio::test]
    async fn store_and_recall() {
        let svc = setup().await;
        let fact = svc.store("mem-agent", "Rust is great", None, "lang").await.unwrap();
        assert!(fact.id > 0);
        assert_eq!(fact.content, "Rust is great");
        assert!(fact.valid_to.is_none());

        let facts = svc.recall("mem-agent", None, Some("lang"), 10).await.unwrap();
        assert!(facts.iter().any(|f| f.content == "Rust is great"));
    }

    #[tokio::test]
    async fn recall_filters_by_project() {
        let svc = setup().await;
        svc.store("mem-agent", "project fact", Some("proj-1"), "").await.unwrap();
        svc.store("mem-agent", "global fact", None, "").await.unwrap();

        let proj_facts = svc.recall("mem-agent", Some("proj-1"), None, 100).await.unwrap();
        assert!(proj_facts.iter().all(|f| f.project_id.as_deref() == Some("proj-1")));
        assert!(proj_facts.iter().any(|f| f.content == "project fact"));
    }

    #[tokio::test]
    async fn recall_excludes_expired() {
        let svc = setup().await;
        let fact = svc.store("mem-agent", "ephemeral", None, "temp").await.unwrap();
        svc.expire(fact.id).await.unwrap();

        let facts = svc.recall("mem-agent", None, Some("temp"), 100).await.unwrap();
        assert!(!facts.iter().any(|f| f.id == fact.id));
    }

    #[tokio::test]
    async fn recall_all_returns_non_expired() {
        let svc = setup().await;
        let f1 = svc.store("mem-agent", "keep me", None, "a").await.unwrap();
        let f2 = svc.store("mem-agent", "expire me", None, "b").await.unwrap();
        svc.expire(f2.id).await.unwrap();

        let all = svc.recall_all("mem-agent").await.unwrap();
        assert!(all.iter().any(|f| f.id == f1.id));
        assert!(!all.iter().any(|f| f.id == f2.id));
    }

    #[tokio::test]
    async fn expire_sets_valid_to() {
        let svc = setup().await;
        let fact = svc.store("mem-agent", "will expire", None, "").await.unwrap();
        assert!(fact.valid_to.is_none());

        svc.expire(fact.id).await.unwrap();

        // Fetch directly to verify valid_to was set
        let row: (Option<f64>,) = sqlx::query_as(
            "SELECT valid_to FROM memory_facts WHERE id = $1"
        )
        .bind(fact.id)
        .fetch_one(&svc.db)
        .await
        .unwrap();
        assert!(row.0.is_some());
    }
}

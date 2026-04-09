/// In-memory agent registry backed by PostgreSQL.
///
/// ┌──────────────────────────────────────────────────────┐
/// │  DashMap<name, AgentEntry>   ← fast reads (hot path) │
/// │        ↕  sync on write                              │
/// │  PostgreSQL agents table     ← persistence           │
/// └──────────────────────────────────────────────────────┘
///
/// Agents self-register on startup and send periodic Heartbeat RPCs.
/// If the gateway restarts the PostgreSQL rows are reloaded into DashMap.

use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use crate::db::Db;
use std::sync::Arc;

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name:         String,
    pub version:      String,
    pub endpoint:     String,
    pub capabilities: Vec<String>,
    pub soul_toml:    String,
    pub last_seen:    Option<String>,
    pub registered_at: String,
}

// ─── Registry ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Registry {
    inner: Arc<DashMap<String, AgentEntry>>,
    db:    Db,
}

impl Registry {
    /// Create registry and reload any existing agents from PostgreSQL.
    pub async fn new(db: Db) -> Result<Self> {
        let reg = Self {
            inner: Arc::new(DashMap::new()),
            db,
        };
        reg.reload_from_db().await?;
        Ok(reg)
    }

    /// Upsert an agent — creates or updates the entry in both cache and DB.
    pub async fn upsert(&self, entry: AgentEntry) -> Result<()> {
        tracing::debug!(agent = %entry.name, endpoint = %entry.endpoint, "registry upsert");
        let caps = serde_json::to_string(&entry.capabilities)?;
        sqlx::query(
            r#"INSERT INTO agents (name, version, endpoint, capabilities, soul_toml, registered_at, last_seen)
               VALUES ($1, $2, $3, $4, $5, $6, $7)
               ON CONFLICT(name) DO UPDATE SET
                 version = excluded.version,
                 endpoint = excluded.endpoint,
                 capabilities = excluded.capabilities,
                 soul_toml = CASE WHEN agents.soul_toml = '' OR agents.soul_toml IS NULL THEN excluded.soul_toml ELSE agents.soul_toml END,
                 last_seen = excluded.last_seen"#,
        )
        .bind(&entry.name)
        .bind(&entry.version)
        .bind(&entry.endpoint)
        .bind(&caps)
        .bind(&entry.soul_toml)
        .bind(&entry.registered_at)
        .bind(&entry.last_seen)
        .execute(&self.db)
        .await?;

        // Ensure memory row exists for this agent
        sqlx::query(
            "INSERT INTO memories (agent_name, content_md) VALUES ($1, '') ON CONFLICT DO NOTHING"
        )
        .bind(&entry.name)
        .execute(&self.db)
        .await?;

        self.inner.insert(entry.name.clone(), entry);
        Ok(())
    }

    /// Update last_seen timestamp (heartbeat).
    pub async fn heartbeat(&self, name: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = sqlx::query("UPDATE agents SET last_seen = $1 WHERE name = $2")
            .bind(&now)
            .bind(name)
            .execute(&self.db)
            .await?
            .rows_affected();

        if rows > 0 {
            if let Some(mut e) = self.inner.get_mut(name) {
                e.last_seen = Some(now);
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn get(&self, name: &str) -> Option<AgentEntry> {
        self.inner.get(name).map(|e| e.clone())
    }

    pub fn list(&self) -> Vec<AgentEntry> {
        self.inner.iter().map(|e| e.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Reload all persisted agents from PostgreSQL into the DashMap cache.
    async fn reload_from_db(&self) -> Result<()> {
        let rows = sqlx::query_as::<_, DbAgent>(
            "SELECT name, version, endpoint, capabilities, soul_toml, last_seen, registered_at FROM agents"
        )
        .fetch_all(&self.db)
        .await?;

        for row in rows {
            let caps: Vec<String> = serde_json::from_str(&row.capabilities).unwrap_or_default();
            self.inner.insert(row.name.clone(), AgentEntry {
                name:          row.name,
                version:       row.version,
                endpoint:      row.endpoint,
                capabilities:  caps,
                soul_toml:     row.soul_toml,
                last_seen:     row.last_seen,
                registered_at: row.registered_at,
            });
        }
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct DbAgent {
    name:          String,
    version:       String,
    endpoint:      String,
    capabilities:  String,
    soul_toml:     String,
    last_seen:     Option<String>,
    registered_at: String,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn make_registry() -> Registry {
        let pool = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        Registry::new(pool).await.unwrap()
    }

    fn entry(name: &str) -> AgentEntry {
        AgentEntry {
            name:          name.into(),
            version:       "0.1.0".into(),
            endpoint:      format!("http://{name}:8080"),
            capabilities:  vec!["test".into()],
            soul_toml:     String::new(),
            last_seen:     None,
            registered_at: Utc::now().to_rfc3339(),
        }
    }

    #[tokio::test]
    async fn insert_and_get() {
        let reg = make_registry().await;
        reg.upsert(entry("alpha")).await.unwrap();
        let got = reg.get("alpha").unwrap();
        assert_eq!(got.endpoint, "http://alpha:8080");
    }

    #[tokio::test]
    async fn upsert_updates_not_duplicates() {
        let reg = make_registry().await;
        reg.upsert(entry("alpha")).await.unwrap();
        let mut e2 = entry("alpha");
        e2.version = "0.2.0".into();
        reg.upsert(e2).await.unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("alpha").unwrap().version, "0.2.0");
    }

    #[tokio::test]
    async fn heartbeat_updates_last_seen() {
        let reg = make_registry().await;
        reg.upsert(entry("beta")).await.unwrap();
        let found = reg.heartbeat("beta").await.unwrap();
        assert!(found);
        assert!(reg.get("beta").unwrap().last_seen.is_some());
    }

    #[tokio::test]
    async fn heartbeat_unknown_agent_returns_false() {
        let reg = make_registry().await;
        let found = reg.heartbeat("nobody").await.unwrap();
        assert!(!found);
    }

    #[tokio::test]
    async fn reload_from_db_on_new_instance() {
        let pool = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        {
            let reg = Registry::new(pool.clone()).await.unwrap();
            reg.upsert(entry("persist-me")).await.unwrap();
        }
        // Create a fresh Registry with the same pool — simulates gateway restart
        let reg2 = Registry::new(pool).await.unwrap();
        assert!(reg2.get("persist-me").is_some());
    }
}

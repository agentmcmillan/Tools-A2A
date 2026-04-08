/// Agent identity store: soul, memory, and todos.
///
/// Rules:
/// - Soul is written once at registration; subsequent writes are ignored
///   unless force=true.
/// - Memory is append-only: content is concatenated, never overwritten.
/// - Todos are never deleted; completion adds a note + timestamp.

use anyhow::{bail, Result};
use chrono::Utc;
use crate::db::Db;

#[derive(Debug, Clone)]
pub struct TodoRow {
    pub id:           i64,
    pub task:         String,
    pub status:       String,
    pub notes:        String,
    pub created_at:   String,
    pub completed_at: Option<String>,
}

pub struct IdentityStore {
    db: Db,
}

impl IdentityStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // ── Soul ─────────────────────────────────────────────────────────────────

    pub async fn get_soul(&self, agent_name: &str) -> Result<String> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT soul_toml FROM agents WHERE name = $1")
                .bind(agent_name)
                .fetch_optional(&self.db)
                .await?;
        match row {
            Some((s,)) => Ok(s),
            None => bail!("agent not found: {agent_name}"),
        }
    }

    /// Set soul content. Silently ignored if already set, unless force=true.
    pub async fn set_soul(&self, agent_name: &str, content: &str, force: bool) -> Result<()> {
        if force {
            sqlx::query("UPDATE agents SET soul_toml = $1 WHERE name = $2")
                .bind(content)
                .bind(agent_name)
                .execute(&self.db)
                .await?;
        } else {
            sqlx::query(
                "UPDATE agents SET soul_toml = $1 WHERE name = $2 AND (soul_toml IS NULL OR soul_toml = '')"
            )
            .bind(content)
            .bind(agent_name)
            .execute(&self.db)
            .await?;
        }
        Ok(())
    }

    // ── Memory ───────────────────────────────────────────────────────────────

    pub async fn get_memory(&self, agent_name: &str) -> Result<String> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT content_md FROM memories WHERE agent_name = $1")
                .bind(agent_name)
                .fetch_optional(&self.db)
                .await?;
        Ok(row.map(|(s,)| s).unwrap_or_default())
    }

    /// Append markdown to the agent's memory (never overwrites).
    pub async fn append_memory(&self, agent_name: &str, content: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO memories (agent_name, content_md) VALUES ($1, $2)
             ON CONFLICT(agent_name) DO UPDATE SET
               content_md = content_md || chr(10) || excluded.content_md"
        )
        .bind(agent_name)
        .bind(content)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    // ── Todos ────────────────────────────────────────────────────────────────

    pub async fn append_todo(&self, agent_name: &str, task: &str) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO todos (agent_name, task, status, created_at)
             VALUES ($1, $2, 'pending', $3)
             RETURNING id"
        )
        .bind(agent_name)
        .bind(task)
        .bind(&now)
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }

    pub async fn complete_todo(&self, id: i64, notes: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        // Annotate + timestamp — the row is NEVER deleted.
        let rows = sqlx::query(
            "UPDATE todos SET status = 'done', notes = $1, completed_at = $2 WHERE id = $3"
        )
        .bind(notes)
        .bind(&now)
        .bind(id)
        .execute(&self.db)
        .await?
        .rows_affected();

        if rows == 0 {
            bail!("todo {id} not found");
        }
        Ok(())
    }

    pub async fn list_todos(
        &self,
        agent_name: &str,
        include_done: bool,
    ) -> Result<Vec<TodoRow>> {
        let rows = if include_done {
            sqlx::query_as::<_, DbTodo>(
                "SELECT id, task, status, notes, created_at, completed_at
                 FROM todos WHERE agent_name = $1 ORDER BY id"
            )
            .bind(agent_name)
            .fetch_all(&self.db)
            .await?
        } else {
            sqlx::query_as::<_, DbTodo>(
                "SELECT id, task, status, notes, created_at, completed_at
                 FROM todos WHERE agent_name = $1 AND status != 'done' ORDER BY id"
            )
            .bind(agent_name)
            .fetch_all(&self.db)
            .await?
        };

        Ok(rows.into_iter().map(|r| TodoRow {
            id:           r.id,
            task:         r.task,
            status:       r.status,
            notes:        r.notes,
            created_at:   r.created_at,
            completed_at: r.completed_at,
        }).collect())
    }
}

#[derive(sqlx::FromRow)]
struct DbTodo {
    id:           i64,
    task:         String,
    status:       String,
    notes:        String,
    created_at:   String,
    completed_at: Option<String>,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::registry::{AgentEntry, Registry};
    use chrono::Utc;

    async fn setup() -> IdentityStore {
        let pool = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let reg = Registry::new(pool.clone()).await.unwrap();
        reg.upsert(AgentEntry {
            name:          "agent-a".into(),
            version:       "0.1.0".into(),
            endpoint:      "http://agent-a:8080".into(),
            capabilities:  vec![],
            soul_toml:     String::new(),
            last_seen:     None,
            registered_at: Utc::now().to_rfc3339(),
        }).await.unwrap();
        IdentityStore::new(pool)
    }

    #[tokio::test]
    async fn soul_set_once_not_overwritten() {
        let store = setup().await;
        store.set_soul("agent-a", "role = 'researcher'", false).await.unwrap();
        store.set_soul("agent-a", "role = 'writer'", false).await.unwrap(); // ignored
        let soul = store.get_soul("agent-a").await.unwrap();
        assert_eq!(soul, "role = 'researcher'");
    }

    #[tokio::test]
    async fn soul_force_overwrites() {
        let store = setup().await;
        store.set_soul("agent-a", "role = 'researcher'", false).await.unwrap();
        store.set_soul("agent-a", "role = 'writer'", true).await.unwrap();
        let soul = store.get_soul("agent-a").await.unwrap();
        assert_eq!(soul, "role = 'writer'");
    }

    #[tokio::test]
    async fn memory_appends_not_overwrites() {
        let store = setup().await;
        store.append_memory("agent-a", "# Day 1").await.unwrap();
        store.append_memory("agent-a", "# Day 2").await.unwrap();
        let mem = store.get_memory("agent-a").await.unwrap();
        assert!(mem.contains("# Day 1"));
        assert!(mem.contains("# Day 2"));
    }

    #[tokio::test]
    async fn todo_never_deleted_on_complete() {
        let store = setup().await;
        let id = store.append_todo("agent-a", "Write tests").await.unwrap();
        store.complete_todo(id, "all green").await.unwrap();

        // Include done=true — row must still be there
        let todos = store.list_todos("agent-a", true).await.unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].status, "done");
        assert_eq!(todos[0].notes, "all green");
        assert!(todos[0].completed_at.is_some());
    }

    #[tokio::test]
    async fn list_todos_excludes_done_by_default() {
        let store = setup().await;
        let id = store.append_todo("agent-a", "Task A").await.unwrap();
        store.append_todo("agent-a", "Task B").await.unwrap();
        store.complete_todo(id, "done").await.unwrap();

        let open = store.list_todos("agent-a", false).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].task, "Task B");
    }
}

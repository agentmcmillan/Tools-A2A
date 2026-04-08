/// Task log: append-only, gateway-signed distributed standup log per project.
///
/// Each write atomically gets the next cursor (MAX(cursor)+1 per project).
/// Entries are never updated or deleted.
///
/// Signatures are HMAC-SHA256 of the canonical string:
///   "{id}|{project_id}|{gateway_name}|{content}|{created_at_secs}"
/// using the gateway's JWT secret.  Peers can verify on receipt.

use crate::db::Db;
use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    Focus,
    Done,
    Blocked,
    Note,
}

impl EntryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Focus   => "focus",
            Self::Done    => "done",
            Self::Blocked => "blocked",
            Self::Note    => "note",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "done"    => Self::Done,
            "blocked" => Self::Blocked,
            "note"    => Self::Note,
            _         => Self::Focus,
        }
    }
}

/// 10-state task lifecycle.
///
/// ```text
///   created ──► queued ──► in_progress ──► completed
///                  │            │                │
///                  │            ├──► blocked ─────┤
///                  │            ├──► waiting ─────┤
///                  │            ├──► delegated    │
///                  └──► cancelled               failed / timed_out
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Created,
    Queued,
    InProgress,
    WaitingForInput,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Delegated,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created         => "created",
            Self::Queued          => "queued",
            Self::InProgress      => "in_progress",
            Self::WaitingForInput => "waiting_for_input",
            Self::Blocked         => "blocked",
            Self::Completed       => "completed",
            Self::Failed          => "failed",
            Self::Cancelled       => "cancelled",
            Self::TimedOut        => "timed_out",
            Self::Delegated       => "delegated",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "created"           => Some(Self::Created),
            "queued"            => Some(Self::Queued),
            "in_progress"       => Some(Self::InProgress),
            "waiting_for_input" => Some(Self::WaitingForInput),
            "blocked"           => Some(Self::Blocked),
            "completed"         => Some(Self::Completed),
            "failed"            => Some(Self::Failed),
            "cancelled"         => Some(Self::Cancelled),
            "timed_out"         => Some(Self::TimedOut),
            "delegated"         => Some(Self::Delegated),
            _                   => None,
        }
    }

    /// Is this a terminal state (no further transitions)?
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled | Self::TimedOut)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id:           String,
    pub project_id:   String,
    pub gateway_name: String,
    pub agent_name:   String,
    pub entry_type:   EntryType,
    pub content:      String,
    pub cursor:       i64,
    pub created_at:   f64,
    pub todo_id:      Option<i64>,
    pub signature:    Option<String>,
    pub task_status:  Option<TaskStatus>,
    pub valid_from:   Option<f64>,
    pub valid_to:     Option<f64>,
}

// ── TaskLog ───────────────────────────────────────────────────────────────────

pub struct TaskLog {
    db:         Db,
    jwt_secret: Vec<u8>,
}

impl TaskLog {
    pub fn new(db: Db, jwt_secret: impl AsRef<[u8]>) -> Self {
        Self {
            db,
            jwt_secret: jwt_secret.as_ref().to_vec(),
        }
    }

    // ── Write ─────────────────────────────────────────────────────────────

    pub async fn append(
        &self,
        project_id:   &str,
        gateway_name: &str,
        agent_name:   &str,
        entry_type:   EntryType,
        content:      &str,
        todo_id:      Option<i64>,
    ) -> Result<LogEntry> {
        self.append_with_status(project_id, gateway_name, agent_name, entry_type, content, todo_id, None, None).await
    }

    /// Append with explicit task status and optional validity window.
    pub async fn append_with_status(
        &self,
        project_id:   &str,
        gateway_name: &str,
        agent_name:   &str,
        entry_type:   EntryType,
        content:      &str,
        todo_id:      Option<i64>,
        task_status:  Option<&TaskStatus>,
        valid_to:     Option<f64>,
    ) -> Result<LogEntry> {
        let id  = Uuid::new_v4().to_string();
        let now = now_secs();

        let sig = self.sign(&id, project_id, gateway_name, content, now);
        let status_str = task_status.map(|s| s.as_str());

        // Atomic cursor increment: subquery in INSERT avoids TOCTOU race
        // when two agents append concurrently to the same project.
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO task_log \
             (id, project_id, gateway_name, agent_name, entry_type, content, cursor, \
              created_at, todo_id, signature, task_status, valid_from, valid_to) \
             VALUES ($1,$2,$3,$4,$5,$6, \
               (SELECT COALESCE(MAX(cursor), 0) + 1 FROM task_log WHERE project_id = $2), \
               $7,$8,$9,$10,$11,$12) \
             RETURNING cursor"
        )
        .bind(&id)
        .bind(project_id)
        .bind(gateway_name)
        .bind(agent_name)
        .bind(entry_type.as_str())
        .bind(content)
        .bind(now)
        .bind(todo_id)
        .bind(&sig)
        .bind(status_str)
        .bind(now)       // valid_from = now
        .bind(valid_to)  // valid_to = caller-specified or NULL
        .fetch_one(&self.db)
        .await
        .context("insert log entry")?;
        let cursor = row.0;

        Ok(LogEntry {
            id,
            project_id: project_id.to_owned(),
            gateway_name: gateway_name.to_owned(),
            agent_name: agent_name.to_owned(),
            entry_type,
            content: content.to_owned(),
            cursor,
            created_at: now,
            todo_id,
            signature: Some(sig),
            task_status: task_status.cloned(),
            valid_from: Some(now),
            valid_to,
        })
    }

    /// Query entries by task status for a project (for reconciler).
    pub async fn by_status(&self, project_id: &str, status: &TaskStatus) -> Result<Vec<LogEntry>> {
        let rows = sqlx::query_as::<_, LogRow>(
            "SELECT * FROM task_log \
             WHERE project_id = $1 AND task_status = $2 \
               AND (valid_to IS NULL OR valid_to > $3) \
             ORDER BY cursor ASC"
        )
        .bind(project_id)
        .bind(status.as_str())
        .bind(now_secs())
        .fetch_all(&self.db)
        .await
        .context("fetch by status")?;
        Ok(rows.into_iter().map(LogEntry::from).collect())
    }

    // ── Read ──────────────────────────────────────────────────────────────

    /// Returns all entries for a project, oldest first.
    pub async fn list(&self, project_id: &str) -> Result<Vec<LogEntry>> {
        self.since(project_id, 0).await
    }

    /// Returns entries with cursor > after_cursor (delta sync).
    pub async fn since(&self, project_id: &str, after_cursor: i64) -> Result<Vec<LogEntry>> {
        let rows = sqlx::query_as::<_, LogRow>(
            "SELECT * FROM task_log \
             WHERE project_id = $1 AND cursor > $2 \
             ORDER BY cursor ASC"
        )
        .bind(project_id)
        .bind(after_cursor)
        .fetch_all(&self.db)
        .await
        .context("fetch log entries")?;
        Ok(rows.into_iter().map(LogEntry::from).collect())
    }

    /// Latest cursor value for a project (0 if none).
    pub async fn head_cursor(&self, project_id: &str) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COALESCE(MAX(cursor), 0) FROM task_log WHERE project_id = $1"
        )
        .bind(project_id)
        .fetch_one(&self.db)
        .await
        .context("head cursor")?;
        Ok(row.0)
    }

    // ── Signature helpers ─────────────────────────────────────────────────

    fn sign(&self, id: &str, project_id: &str, gateway: &str, content: &str, ts: f64) -> String {
        let msg = format!("{id}|{project_id}|{gateway}|{content}|{}", ts as u64);
        let mut mac = HmacSha256::new_from_slice(&self.jwt_secret).expect("HMAC key");
        mac.update(msg.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    pub fn verify_entry(&self, entry: &LogEntry) -> bool {
        let Some(sig) = &entry.signature else { return false; };
        let expected = self.sign(
            &entry.id,
            &entry.project_id,
            &entry.gateway_name,
            &entry.content,
            entry.created_at,
        );
        sig == &expected
    }
}

// ── SQLx row mapping ──────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct LogRow {
    id:           String,
    project_id:   String,
    gateway_name: String,
    agent_name:   String,
    entry_type:   String,
    content:      String,
    cursor:       i64,
    created_at:   f64,
    todo_id:      Option<i64>,
    signature:    Option<String>,
    task_status:  Option<String>,
    valid_from:   Option<f64>,
    valid_to:     Option<f64>,
}

impl From<LogRow> for LogEntry {
    fn from(r: LogRow) -> Self {
        Self {
            id:           r.id,
            project_id:   r.project_id,
            gateway_name: r.gateway_name,
            agent_name:   r.agent_name,
            entry_type:   EntryType::from_str(&r.entry_type),
            content:      r.content,
            cursor:       r.cursor,
            created_at:   r.created_at,
            todo_id:      r.todo_id,
            signature:    r.signature,
            task_status:  r.task_status.and_then(|s| TaskStatus::from_str(&s)),
            valid_from:   r.valid_from,
            valid_to:     r.valid_to,
        }
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn log() -> TaskLog {
        let db = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        TaskLog::new(db, b"test-secret")
    }

    async fn project(db: &Db) -> String {
        let id = Uuid::new_v4().to_string();
        let now = now_secs();
        sqlx::query(
            "INSERT INTO projects (id,name,repo_url,description,visibility,folder_path,file_glob,\
             is_active,created_at,updated_at,origin_gateway,peer_gateway,peer_project_id) \
             VALUES ($1,$2,$3,'','lan-only','','**/*',0,$4,$5,'gw','','')"
        )
        .bind(&id).bind(&id).bind("https://x.com").bind(now).bind(now)
        .execute(db)
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn append_and_list() {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let log = TaskLog::new(db.clone(), b"secret");
        let pid = project(&db).await;

        let e = log.append(&pid, "site-a", "orch", EntryType::Focus, "working on auth", None).await.unwrap();
        assert_eq!(e.cursor, 1);

        let entries = log.list(&pid).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "working on auth");
    }

    #[tokio::test]
    async fn cursors_are_monotonic() {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let log = TaskLog::new(db.clone(), b"secret");
        let pid = project(&db).await;

        for i in 0..5 {
            log.append(&pid, "site-a", "agent", EntryType::Note, &format!("entry {i}"), None).await.unwrap();
        }
        let entries = log.list(&pid).await.unwrap();
        let cursors: Vec<i64> = entries.iter().map(|e| e.cursor).collect();
        assert_eq!(cursors, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn delta_sync() {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let log = TaskLog::new(db.clone(), b"secret");
        let pid = project(&db).await;

        for i in 0..4 {
            log.append(&pid, "site-a", "a", EntryType::Focus, &format!("e{i}"), None).await.unwrap();
        }
        let delta = log.since(&pid, 2).await.unwrap();
        assert_eq!(delta.len(), 2);
        assert_eq!(delta[0].cursor, 3);
    }

    #[tokio::test]
    async fn signature_verifies() {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let log = TaskLog::new(db.clone(), b"my-secret");
        let pid = project(&db).await;
        let e   = log.append(&pid, "gw", "agent", EntryType::Focus, "hello", None).await.unwrap();
        assert!(log.verify_entry(&e));
    }

    #[tokio::test]
    async fn different_projects_independent_cursors() {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let log = TaskLog::new(db.clone(), b"s");
        let p1  = project(&db).await;
        let p2  = project(&db).await;
        log.append(&p1, "gw", "a", EntryType::Focus, "one", None).await.unwrap();
        log.append(&p1, "gw", "a", EntryType::Focus, "two", None).await.unwrap();
        let e = log.append(&p2, "gw", "a", EntryType::Focus, "first", None).await.unwrap();
        assert_eq!(e.cursor, 1); // p2 starts at 1, not 3
    }
}

/// File sync engine — snapshot creation, diff computation, and push/pull helpers.
///
/// Flow for a local project:
///   1. scan_and_snapshot() → walks folder_path, stores objects, writes snapshot row
///   2. NATS publishes "a2a.projects.{id}.snapshot" notification
///   3. Peers receive notification, call diff_against_head() to see what changed
///   4. Peers fetch missing objects, apply snapshot
///
/// Flow for a peer project update:
///   1. Peer sends snapshot + missing objects list
///   2. We fetch the objects
///   3. apply_snapshot() stores the snapshot row and advances project_heads

use crate::db::Db;
use crate::object_store::{build_transcript, scan_directory, transcript_id, ObjectStore, TranscriptEntry};
use crate::projects::ProjectStore;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id:         String,     // sha256 of transcript
    pub project_id: String,
    pub gateway:    String,
    pub transcript: String,
    pub created_at: f64,
}

/// Diff between two snapshots — used to decide what objects to transfer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotDiff {
    pub added:    Vec<TranscriptEntry>,   // in new, not in old
    pub removed:  Vec<TranscriptEntry>,   // in old, not in new
    pub modified: Vec<(TranscriptEntry, TranscriptEntry)>, // (old, new) same path, different hash
}

impl SnapshotDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }

    /// All object hashes that must be fetched to apply this diff.
    pub fn required_objects(&self) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.added    { out.push(e.sha256.clone()); }
        for (_, n) in &self.modified { out.push(n.sha256.clone()); }
        out
    }
}

// ── FileSyncEngine ────────────────────────────────────────────────────────────

pub struct FileSyncEngine {
    db:       Db,
    objects:  ObjectStore,
    projects: ProjectStore,
    gateway:  String,
}

impl FileSyncEngine {
    pub fn new(
        db:       Db,
        objects:  ObjectStore,
        gateway:  impl Into<String>,
    ) -> Self {
        let projects = ProjectStore::new(db.clone());
        Self { db, objects, projects, gateway: gateway.into() }
    }

    // ── Snapshot creation ─────────────────────────────────────────────────

    /// Walk the project's folder_path, store all objects, write a snapshot row.
    /// Returns None if the project folder hasn't been configured yet.
    pub async fn scan_and_snapshot(&self, project_id: &str) -> Result<Option<Snapshot>> {
        let project = self.projects.get(project_id).await?
            .context("project not found")?;

        if project.folder_path.is_empty() {
            return Ok(None);
        }

        let path = Path::new(&project.folder_path);
        if !path.exists() {
            bail!("project folder does not exist: {}", project.folder_path);
        }

        let entries    = scan_directory(path, &self.objects, &project.file_glob).await?;
        let transcript = build_transcript(entries);
        let snap_id    = transcript_id(&transcript);

        // Check if this snapshot already exists (no-op if identical)
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM snapshots WHERE id = $1"
        )
        .bind(&snap_id)
        .fetch_optional(&self.db)
        .await?;

        if existing.is_some() {
            return self.get_snapshot(&snap_id).await;
        }

        let snap = self.write_snapshot(project_id, &snap_id, &transcript).await?;
        self.advance_head(project_id, &snap_id).await?;
        Ok(Some(snap))
    }

    /// Write a snapshot received from a peer (objects must already be stored).
    pub async fn apply_peer_snapshot(
        &self,
        project_id:  &str,
        snap_id:     &str,
        transcript:  &str,
        from_gateway: &str,
    ) -> Result<Snapshot> {
        let snap = Snapshot {
            id:         snap_id.to_owned(),
            project_id: project_id.to_owned(),
            gateway:    from_gateway.to_owned(),
            transcript: transcript.to_owned(),
            created_at: now_secs(),
        };

        sqlx::query(
            "INSERT INTO snapshots (id, project_id, gateway, transcript, created_at) \
             VALUES ($1,$2,$3,$4,$5)"
        )
        .bind(&snap.id)
        .bind(&snap.project_id)
        .bind(&snap.gateway)
        .bind(&snap.transcript)
        .bind(snap.created_at)
        .execute(&self.db)
        .await
        .context("insert peer snapshot")?;

        self.advance_head(project_id, snap_id).await?;
        Ok(snap)
    }

    // ── Diff ──────────────────────────────────────────────────────────────

    /// Compute diff between current HEAD and a new snapshot transcript.
    pub async fn diff_against_head(&self, project_id: &str, new_transcript: &str) -> Result<SnapshotDiff> {
        let old_transcript = self.head_transcript(project_id).await?;
        Ok(compute_diff(old_transcript.as_deref(), new_transcript))
    }

    /// Diff between two arbitrary transcripts.
    pub fn diff(old: Option<&str>, new: &str) -> SnapshotDiff {
        compute_diff(old, new)
    }

    // ── Read ──────────────────────────────────────────────────────────────

    pub async fn head(&self, project_id: &str) -> Result<Option<Snapshot>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT snapshot_id FROM project_heads WHERE project_id = $1"
        )
        .bind(project_id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            None => Ok(None),
            Some((snap_id,)) => self.get_snapshot(&snap_id).await,
        }
    }

    pub async fn get_snapshot(&self, snap_id: &str) -> Result<Option<Snapshot>> {
        let row = sqlx::query_as::<_, SnapRow>(
            "SELECT * FROM snapshots WHERE id = $1"
        )
        .bind(snap_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(Snapshot::from))
    }

    pub async fn list_snapshots(&self, project_id: &str) -> Result<Vec<Snapshot>> {
        let rows = sqlx::query_as::<_, SnapRow>(
            "SELECT * FROM snapshots WHERE project_id = $1 ORDER BY created_at DESC"
        )
        .bind(project_id)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(Snapshot::from).collect())
    }

    // ── Object access ─────────────────────────────────────────────────────

    pub fn objects(&self) -> &ObjectStore {
        &self.objects
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    // ── Private ───────────────────────────────────────────────────────────

    async fn write_snapshot(&self, project_id: &str, snap_id: &str, transcript: &str) -> Result<Snapshot> {
        let now = now_secs();
        sqlx::query(
            "INSERT INTO snapshots (id, project_id, gateway, transcript, created_at) \
             VALUES ($1,$2,$3,$4,$5)"
        )
        .bind(snap_id)
        .bind(project_id)
        .bind(&self.gateway)
        .bind(transcript)
        .bind(now)
        .execute(&self.db)
        .await
        .context("write snapshot")?;

        Ok(Snapshot {
            id:         snap_id.to_owned(),
            project_id: project_id.to_owned(),
            gateway:    self.gateway.clone(),
            transcript: transcript.to_owned(),
            created_at: now,
        })
    }

    async fn advance_head(&self, project_id: &str, snap_id: &str) -> Result<()> {
        let now = now_secs();
        sqlx::query(
            "INSERT INTO project_heads (project_id, snapshot_id, updated_at) VALUES ($1,$2,$3) \
             ON CONFLICT(project_id) DO UPDATE SET snapshot_id=excluded.snapshot_id, updated_at=excluded.updated_at"
        )
        .bind(project_id)
        .bind(snap_id)
        .bind(now)
        .execute(&self.db)
        .await
        .context("advance project head")?;
        Ok(())
    }

    async fn head_transcript(&self, project_id: &str) -> Result<Option<String>> {
        let snap = self.head(project_id).await?;
        Ok(snap.map(|s| s.transcript))
    }
}

// ── Diff algorithm ────────────────────────────────────────────────────────────

use crate::object_store::parse_transcript;
use std::collections::HashMap;
use crate::util::now_secs;

fn compute_diff(old_text: Option<&str>, new_text: &str) -> SnapshotDiff {
    let old_entries: HashMap<String, TranscriptEntry> = old_text
        .map(|t| parse_transcript(t).into_iter().map(|e| (e.path.clone(), e)).collect())
        .unwrap_or_default();

    let new_entries: HashMap<String, TranscriptEntry> = parse_transcript(new_text)
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    let mut diff = SnapshotDiff::default();

    for (path, new_e) in &new_entries {
        match old_entries.get(path) {
            None => diff.added.push(new_e.clone()),
            Some(old_e) if old_e.sha256 != new_e.sha256 => {
                diff.modified.push((old_e.clone(), new_e.clone()));
            }
            _ => {}
        }
    }

    for (path, old_e) in &old_entries {
        if !new_entries.contains_key(path) {
            diff.removed.push(old_e.clone());
        }
    }

    // Sort for determinism
    diff.added.sort_by(|a, b| a.path.cmp(&b.path));
    diff.removed.sort_by(|a, b| a.path.cmp(&b.path));
    diff.modified.sort_by(|a, b| a.0.path.cmp(&b.0.path));

    diff
}

// ── SQLx row ─────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct SnapRow {
    id:         String,
    project_id: String,
    gateway:    String,
    transcript: String,
    created_at: f64,
}

impl From<SnapRow> for Snapshot {
    fn from(r: SnapRow) -> Self {
        Self {
            id:         r.id,
            project_id: r.project_id,
            gateway:    r.gateway,
            transcript: r.transcript,
            created_at: r.created_at,
        }
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::TempDir;
    use tokio::fs;

    async fn engine(tmp: &TempDir) -> FileSyncEngine {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let obj = ObjectStore::new(db.clone(), tmp.path().join("objects"));
        FileSyncEngine::new(db, obj, "site-a")
    }

    async fn make_project(engine: &FileSyncEngine, folder: &Path) -> String {
        let db  = &engine.db;
        let id  = uuid::Uuid::new_v4().to_string();
        let now = now_secs();
        sqlx::query(
            "INSERT INTO projects \
             (id,name,repo_url,description,visibility,folder_path,file_glob,is_active,\
              created_at,updated_at,origin_gateway,peer_gateway,peer_project_id) \
             VALUES ($1,$2,'https://x.com','','lan-only',$3,'**/*',0,$4,$5,'gw','','')"
        )
        .bind(&id).bind(&id).bind(folder.to_str().unwrap()).bind(now).bind(now)
        .execute(db).await.unwrap();
        id
    }

    #[tokio::test]
    async fn scan_creates_snapshot() {
        let tmp  = TempDir::new().unwrap();
        let eng  = engine(&tmp).await;
        let dir  = tmp.path().join("proj");
        fs::create_dir_all(&dir).await.unwrap();
        fs::write(dir.join("a.txt"), b"hello").await.unwrap();
        fs::write(dir.join("b.txt"), b"world").await.unwrap();

        let pid  = make_project(&eng, &dir).await;
        let snap = eng.scan_and_snapshot(&pid).await.unwrap().unwrap();
        assert!(!snap.id.is_empty());
        assert!(snap.transcript.contains("a.txt"));
        assert!(snap.transcript.contains("b.txt"));
    }

    #[tokio::test]
    async fn scan_idempotent() {
        let tmp  = TempDir::new().unwrap();
        let eng  = engine(&tmp).await;
        let dir  = tmp.path().join("proj2");
        fs::create_dir_all(&dir).await.unwrap();
        fs::write(dir.join("x.txt"), b"data").await.unwrap();

        let pid  = make_project(&eng, &dir).await;
        let s1   = eng.scan_and_snapshot(&pid).await.unwrap().unwrap();
        let s2   = eng.scan_and_snapshot(&pid).await.unwrap().unwrap();
        assert_eq!(s1.id, s2.id);
    }

    #[tokio::test]
    async fn diff_detects_changes() {
        let old = "F|a.txt|aaa|10|644|100\nF|b.txt|bbb|20|644|200";
        let new = "F|a.txt|ccc|10|644|300\nF|c.txt|ddd|30|644|400";
        let diff = FileSyncEngine::diff(Some(old), new);
        assert_eq!(diff.added.len(), 1);    // c.txt
        assert_eq!(diff.removed.len(), 1);  // b.txt
        assert_eq!(diff.modified.len(), 1); // a.txt changed hash
    }

    #[tokio::test]
    async fn diff_required_objects() {
        let old = "F|a.txt|aaa|10|644|100";
        let new = "F|a.txt|bbb|10|644|200\nF|z.txt|zzz|5|644|300";
        let diff = FileSyncEngine::diff(Some(old), new);
        let objs = diff.required_objects();
        assert!(objs.contains(&"bbb".to_string()));
        assert!(objs.contains(&"zzz".to_string()));
    }
}

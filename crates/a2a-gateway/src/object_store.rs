/// Content-addressed object store.
///
/// Objects are identified by sha256 of their content.
/// Layout on disk:  data/objects/{sha256[0..2]}/{sha256[2..]}
///
/// This mirrors git's loose-object layout.
///
///   hash = "a3f9..."
///   path = data/objects/a3/f9...
///
/// Objects are immutable once written; write is a no-op if hash already exists.

use crate::db::Db;
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

// ── ObjectStore ───────────────────────────────────────────────────────────────

pub struct ObjectStore {
    db:      Db,
    root:    PathBuf,   // e.g. "data/objects"
}

impl ObjectStore {
    pub fn new(db: Db, root: impl Into<PathBuf>) -> Self {
        Self { db, root: root.into() }
    }

    /// Hash content and store if not already present. Returns sha256 hex.
    pub async fn put(&self, content: &[u8]) -> Result<String> {
        let hash = sha256_hex(content);
        if self.exists_db(&hash).await? {
            return Ok(hash);
        }

        let path = self.object_path(&hash);
        let dir  = path.parent().unwrap();
        fs::create_dir_all(dir).await.context("create object dir")?;
        fs::write(&path, content).await.context("write object")?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        sqlx::query(
            "INSERT INTO objects (sha256, size_bytes, created_at) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING"
        )
        .bind(&hash)
        .bind(content.len() as i64)
        .bind(now)
        .execute(&self.db)
        .await
        .context("insert object record")?;

        Ok(hash)
    }

    /// Fetch object bytes by hash.
    pub async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        Self::validate_hash(hash)?;
        let path = self.object_path(hash);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).await.context("read object")?;
        Ok(Some(bytes))
    }

    /// True if the object is already stored (fast DB check, no disk I/O).
    pub async fn exists(&self, hash: &str) -> Result<bool> {
        self.exists_db(hash).await
    }

    /// Returns hashes present in `want` but missing locally.
    pub async fn missing(&self, want: &[String]) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        for h in want {
            if !self.exists_db(h).await? {
                missing.push(h.clone());
            }
        }
        Ok(missing)
    }

    fn object_path(&self, hash: &str) -> PathBuf {
        self.root.join(&hash[..2]).join(&hash[2..])
    }

    /// Validate that a hash is a proper hex string (prevents path traversal SEC-06).
    fn validate_hash(hash: &str) -> Result<()> {
        if hash.len() < 4 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("invalid object hash: must be hex string, got '{}'", &hash[..hash.len().min(20)]);
        }
        Ok(())
    }

    async fn exists_db(&self, hash: &str) -> Result<bool> {
        let count: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM objects WHERE sha256 = $1"
        )
        .bind(hash)
        .fetch_one(&self.db)
        .await
        .context("exists check")?;
        Ok(count.0 > 0)
    }
}

// ── Transcript helpers ────────────────────────────────────────────────────────

/// One entry in a snapshot transcript.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptEntry {
    pub path:   String,
    pub sha256: String,
    pub size:   u64,
    pub mode:   u32,
    pub mtime:  u64,
}

impl TranscriptEntry {
    /// Serialise to a single line: F | path | sha256 | size | mode | mtime
    pub fn to_line(&self) -> String {
        format!("F|{}|{}|{}|{:o}|{}", self.path, self.sha256, self.size, self.mode, self.mtime)
    }

    pub fn from_line(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() != 6 || parts[0] != "F" {
            return None;
        }
        Some(Self {
            path:   parts[1].to_owned(),
            sha256: parts[2].to_owned(),
            size:   parts[3].parse().ok()?,
            mode:   u32::from_str_radix(parts[4], 8).ok()?,
            mtime:  parts[5].parse().ok()?,
        })
    }
}

/// Build a sorted transcript text from a list of entries.
pub fn build_transcript(mut entries: Vec<TranscriptEntry>) -> String {
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries.iter().map(|e| e.to_line()).collect::<Vec<_>>().join("\n")
}

/// Parse a transcript text back into entries.
pub fn parse_transcript(text: &str) -> Vec<TranscriptEntry> {
    text.lines().filter_map(TranscriptEntry::from_line).collect()
}

/// Hash a transcript to produce a stable snapshot ID.
pub fn transcript_id(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

// ── Scan helpers ──────────────────────────────────────────────────────────────

/// Recursively scan a directory, returning transcript entries for all files.
/// Files are filtered by a simple glob pattern (only `**/*` and `*.ext` supported here).
pub async fn scan_directory(
    root:    &Path,
    store:   &ObjectStore,
    glob:    &str,
) -> Result<Vec<TranscriptEntry>> {
    let mut entries = Vec::new();
    scan_dir_recursive(root, root, store, glob, &mut entries).await?;
    Ok(entries)
}

fn matches_glob(rel_path: &str, glob: &str) -> bool {
    // Minimal glob: "**/*" matches everything, "*.rs" matches extension, exact otherwise
    if glob == "**/*" || glob == "*" {
        return true;
    }
    if glob.starts_with("*.") {
        let ext = &glob[1..];
        return rel_path.ends_with(ext);
    }
    rel_path == glob
}

#[async_recursion::async_recursion]
async fn scan_dir_recursive(
    root:    &Path,
    dir:     &Path,
    store:   &ObjectStore,
    glob:    &str,
    out:     &mut Vec<TranscriptEntry>,
) -> Result<()> {
    let mut read_dir = fs::read_dir(dir).await.context("read dir")?;
    while let Some(entry) = read_dir.next_entry().await? {
        let path = entry.path();
        let meta = entry.metadata().await?;
        if meta.is_dir() {
            scan_dir_recursive(root, &path, store, glob, out).await?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().to_string();
            if !matches_glob(&rel, glob) {
                continue;
            }
            let content = fs::read(&path).await.context("read file for scan")?;
            let sha256  = store.put(&content).await?;
            let mtime   = meta.modified()
                .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
                .unwrap_or(0);
            out.push(TranscriptEntry {
                path: rel,
                sha256,
                size: meta.len(),
                mode: 0o644,
                mtime,
            });
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::TempDir;

    async fn store_in_temp() -> (ObjectStore, TempDir) {
        let tmp  = TempDir::new().unwrap();
        let db   = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let obj  = ObjectStore::new(db, tmp.path().join("objects"));
        (obj, tmp)
    }

    #[tokio::test]
    async fn put_and_get_roundtrip() {
        let (store, _tmp) = store_in_temp().await;
        let data  = b"hello world";
        let hash  = store.put(data).await.unwrap();
        assert_eq!(hash.len(), 64);
        let got   = store.get(&hash).await.unwrap().unwrap();
        assert_eq!(got, data);
    }

    #[tokio::test]
    async fn idempotent_put() {
        let (store, _tmp) = store_in_temp().await;
        let h1 = store.put(b"abc").await.unwrap();
        let h2 = store.put(b"abc").await.unwrap();
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn missing_detection() {
        let (store, _tmp) = store_in_temp().await;
        let h = store.put(b"present").await.unwrap();
        let want = vec![h.clone(), "deadbeef".repeat(8)];
        let miss  = store.missing(&want).await.unwrap();
        assert_eq!(miss.len(), 1);
        assert!(miss[0].contains("dead"));
    }

    #[tokio::test]
    async fn transcript_roundtrip() {
        let entries = vec![
            TranscriptEntry { path: "b.rs".into(), sha256: "aa".repeat(32), size: 100, mode: 0o644, mtime: 1234 },
            TranscriptEntry { path: "a.rs".into(), sha256: "bb".repeat(32), size: 200, mode: 0o644, mtime: 5678 },
        ];
        let text   = build_transcript(entries.clone());
        // sorted: a.rs first
        assert!(text.starts_with("F|a.rs|"));
        let parsed = parse_transcript(&text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, "a.rs");
    }
}

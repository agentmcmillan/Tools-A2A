/// Phase 2 integration tests — File Sync (content-addressed object store)
///
/// Tests the ObjectStore and FileSyncEngine:
///   - put/get round-trip
///   - idempotent puts (same content → same hash, stored once)
///   - missing objects query
///   - transcript build + snapshot creation
///   - diff detection between snapshots

mod helpers;

use a2a_gateway::{
    db,
    object_store::{ObjectStore, TranscriptEntry, build_transcript, transcript_id, parse_transcript},
    file_sync::FileSyncEngine,
    projects::ProjectStore,
};
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn engine_and_project(tmp: &TempDir) -> (FileSyncEngine, String, a2a_gateway::db::Db) {
    let db  = db::open(":memory:").await.unwrap();
    let obj = ObjectStore::new(db.clone(), tmp.path().join(".objects").to_string_lossy().into_owned());
    let eng = FileSyncEngine::new(db.clone(), obj, "site-a");
    let pstore = ProjectStore::new(db.clone());
    let pid = pstore.create("myproj", "https://x.com", "", "site-a", "", "").await.unwrap().id;
    pstore.set_folder_path(&pid, &tmp.path().to_string_lossy()).await.unwrap();
    pstore.set_file_glob(&pid, "**/*").await.unwrap();
    (eng, pid, db)
}

// ── Object store ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn put_and_get_round_trip() {
    let tmp = TempDir::new().unwrap();
    let db  = db::open(":memory:").await.unwrap();
    let obj = ObjectStore::new(db, tmp.path().to_string_lossy().into_owned());

    let data = b"hello world";
    let hash = obj.put(data).await.unwrap();
    assert_eq!(hash.len(), 64); // sha256 hex

    let retrieved = obj.get(&hash).await.unwrap().expect("should exist");
    assert_eq!(retrieved, data);
}

#[tokio::test]
async fn put_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let db  = db::open(":memory:").await.unwrap();
    let obj = ObjectStore::new(db, tmp.path().to_string_lossy().into_owned());

    let h1 = obj.put(b"same content").await.unwrap();
    let h2 = obj.put(b"same content").await.unwrap();
    assert_eq!(h1, h2);
}

#[tokio::test]
async fn missing_objects_query() {
    let tmp = TempDir::new().unwrap();
    let db  = db::open(":memory:").await.unwrap();
    let obj = ObjectStore::new(db, tmp.path().to_string_lossy().into_owned());

    let h_present = obj.put(b"i am here").await.unwrap();
    let h_absent  = "a".repeat(64);

    let missing = obj.missing(&[h_present.clone(), h_absent.clone()]).await.unwrap();
    assert!(!missing.contains(&h_present));
    assert!(missing.contains(&h_absent));
}

#[tokio::test]
async fn get_nonexistent_returns_none() {
    let tmp = TempDir::new().unwrap();
    let db  = db::open(":memory:").await.unwrap();
    let obj = ObjectStore::new(db, tmp.path().to_string_lossy().into_owned());

    let result = obj.get(&"b".repeat(64)).await.unwrap();
    assert!(result.is_none());
}

// ── Transcript ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn transcript_sorted_by_path() {
    let entries = vec![
        TranscriptEntry { path: "z.txt".into(), sha256: "z".repeat(64), size: 1, mode: 0o644, mtime: 0 },
        TranscriptEntry { path: "a.txt".into(), sha256: "a".repeat(64), size: 1, mode: 0o644, mtime: 0 },
        TranscriptEntry { path: "m.txt".into(), sha256: "m".repeat(64), size: 1, mode: 0o644, mtime: 0 },
    ];
    let text  = build_transcript(entries);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains("a.txt"));
    assert!(lines[1].contains("m.txt"));
    assert!(lines[2].contains("z.txt"));
}

#[tokio::test]
async fn transcript_round_trip_parse() {
    let entries = vec![
        TranscriptEntry { path: "src/main.rs".into(), sha256: "f".repeat(64), size: 1024, mode: 0o644, mtime: 1700000000 },
        TranscriptEntry { path: "Cargo.toml".into(),  sha256: "e".repeat(64), size: 256,  mode: 0o644, mtime: 1700000001 },
    ];
    let text   = build_transcript(entries);
    let parsed = parse_transcript(&text);
    assert_eq!(parsed.len(), 2);
    let cargo = parsed.iter().find(|e| e.path == "Cargo.toml").unwrap();
    assert_eq!(cargo.size, 256);
}

#[tokio::test]
async fn transcript_id_deterministic() {
    let entries = vec![
        TranscriptEntry { path: "a.rs".into(), sha256: "a".repeat(64), size: 1, mode: 0o644, mtime: 0 },
    ];
    let text = build_transcript(entries);
    let id1  = transcript_id(&text);
    let id2  = transcript_id(&text);
    assert_eq!(id1, id2);
    assert_ne!(id1, text);
}

// ── FileSyncEngine ────────────────────────────────────────────────────────────

#[tokio::test]
async fn scan_creates_snapshot() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), b"hello").unwrap();
    std::fs::write(tmp.path().join("world.txt"), b"world").unwrap();

    let (engine, pid, _db) = engine_and_project(&tmp).await;

    let snap = engine.scan_and_snapshot(&pid).await.unwrap().expect("should return snapshot");
    assert!(!snap.id.is_empty());

    let head = engine.head(&pid).await.unwrap();
    assert!(head.is_some());
    assert_eq!(head.unwrap().id, snap.id);
}

#[tokio::test]
async fn idempotent_scan_same_snapshot() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("file.txt"), b"content").unwrap();

    let (engine, pid, _db) = engine_and_project(&tmp).await;

    let s1 = engine.scan_and_snapshot(&pid).await.unwrap().unwrap();
    let s2 = engine.scan_and_snapshot(&pid).await.unwrap().unwrap();
    assert_eq!(s1.id, s2.id, "same content must produce same snapshot id");
}

#[tokio::test]
async fn different_content_different_snapshot() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("original.txt"), b"original").unwrap();

    let (engine, pid, _db) = engine_and_project(&tmp).await;
    let s1 = engine.scan_and_snapshot(&pid).await.unwrap().unwrap();

    // Add a new file — should produce different snapshot
    std::fs::write(tmp.path().join("added.txt"), b"new content").unwrap();
    let s2 = engine.scan_and_snapshot(&pid).await.unwrap().unwrap();

    assert_ne!(s1.id, s2.id);
}

#[tokio::test]
async fn diff_against_head_detects_changes() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("file.txt"), b"original").unwrap();

    let (engine, pid, _db) = engine_and_project(&tmp).await;
    let snap1 = engine.scan_and_snapshot(&pid).await.unwrap().unwrap();

    // Build a new transcript manually for the diff
    std::fs::write(tmp.path().join("file2.txt"), b"added").unwrap();
    let snap2 = engine.scan_and_snapshot(&pid).await.unwrap().unwrap();

    // diff_against_head needs the *old* head vs new transcript
    // After second scan, head is snap2. Diff snap1's transcript vs snap2
    let diff = engine.diff_against_head(&pid, &snap1.transcript).await.unwrap();
    // snap1 doesn't have file2.txt; head (snap2) does — so file2.txt appears as removed from new's perspective
    // Actually: diff_against_head(pid, new_transcript) compares head vs new_transcript
    // head is snap2 (has both files), new_transcript is snap1 (only original.txt)
    // → file2.txt would be "added to old vs new" = removed in new
    assert!(!diff.is_empty(), "should detect difference between transcripts");
}

#[tokio::test]
async fn required_objects_from_diff() {
    use a2a_gateway::file_sync::FileSyncEngine;

    // Use the static diff() method with hand-crafted transcripts
    let old_text = build_transcript(vec![
        TranscriptEntry { path: "a.txt".into(), sha256: "a".repeat(64), size: 1, mode: 0o644, mtime: 0 },
    ]);
    let new_text = build_transcript(vec![
        TranscriptEntry { path: "a.txt".into(), sha256: "a".repeat(64), size: 1, mode: 0o644, mtime: 0 },
        TranscriptEntry { path: "b.txt".into(), sha256: "b".repeat(64), size: 2, mode: 0o644, mtime: 0 },
    ]);

    let diff = FileSyncEngine::diff(Some(&old_text), &new_text);
    let needed = diff.required_objects();

    assert_eq!(needed.len(), 1, "only b.txt was added");
    assert!(needed[0].starts_with("bbbb"));
}

/// Phase 2 integration tests — Task Log
///
/// Tests the TaskLog data layer:
///   - append + list
///   - monotonic cursor ordering
///   - delta sync via `since()`
///   - HMAC signature round-trip
///   - independent cursors per project

mod helpers;

use a2a_gateway::{db, task_log::{EntryType, TaskLog}, projects::ProjectStore};

async fn setup() -> (TaskLog, String, a2a_gateway::db::Db) {
    let db       = db::open(":memory:").await.unwrap();
    let tl       = TaskLog::new(db.clone(), b"test-secret");
    let projects = ProjectStore::new(db.clone());
    let pid      = projects.create("test-proj", "https://x.com", "", "site-a", "", "").await.unwrap().id;
    (tl, pid, db)
}

#[tokio::test]
async fn append_and_list() {
    let (tl, pid, _db) = setup().await;

    tl.append(&pid, "site-a", "orchestrator", EntryType::Focus, "build the auth module", None).await.unwrap();
    tl.append(&pid, "site-a", "research",     EntryType::Note,  "auth: JWT vs session",  None).await.unwrap();

    let entries = tl.since(&pid, 0).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| e.content == "build the auth module"));
    assert!(entries.iter().any(|e| e.content == "auth: JWT vs session"));
}

#[tokio::test]
async fn cursors_are_monotonically_increasing() {
    let (tl, pid, _db) = setup().await;

    for i in 0..5u32 {
        tl.append(&pid, "site-a", "agent", EntryType::Note, &format!("entry {i}"), None).await.unwrap();
    }

    let entries = tl.since(&pid, 0).await.unwrap();
    assert_eq!(entries.len(), 5);
    for w in entries.windows(2) {
        assert!(w[1].cursor > w[0].cursor, "cursors must be strictly increasing");
    }
}

#[tokio::test]
async fn delta_sync_since_cursor() {
    let (tl, pid, _db) = setup().await;

    tl.append(&pid, "site-a", "agent", EntryType::Note, "old entry", None).await.unwrap();
    let first = tl.since(&pid, 0).await.unwrap();
    let first_cursor = first[0].cursor;

    tl.append(&pid, "site-a", "agent", EntryType::Note, "new entry 1", None).await.unwrap();
    tl.append(&pid, "site-a", "agent", EntryType::Note, "new entry 2", None).await.unwrap();

    let delta = tl.since(&pid, first_cursor).await.unwrap();
    assert_eq!(delta.len(), 2, "should only return entries after given cursor");
    assert!(delta.iter().all(|e| e.content.starts_with("new")));
}

#[tokio::test]
async fn head_cursor_tracks_max() {
    let (tl, pid, _db) = setup().await;

    tl.append(&pid, "site-a", "agent", EntryType::Note, "a", None).await.unwrap();
    tl.append(&pid, "site-a", "agent", EntryType::Note, "b", None).await.unwrap();
    tl.append(&pid, "site-a", "agent", EntryType::Note, "c", None).await.unwrap();

    let head = tl.head_cursor(&pid).await.unwrap();
    assert_eq!(head, 3);
}

#[tokio::test]
async fn independent_cursors_per_project() {
    let db       = db::open(":memory:").await.unwrap();
    let tl       = TaskLog::new(db.clone(), b"test-secret");
    let projects = ProjectStore::new(db.clone());

    let pa = projects.create("proj-a", "https://a.com", "", "site-a", "", "").await.unwrap().id;
    let pb = projects.create("proj-b", "https://b.com", "", "site-a", "", "").await.unwrap().id;

    tl.append(&pa, "site-a", "agent", EntryType::Note, "a1", None).await.unwrap();
    tl.append(&pa, "site-a", "agent", EntryType::Note, "a2", None).await.unwrap();
    tl.append(&pb, "site-a", "agent", EntryType::Note, "b1", None).await.unwrap();

    assert_eq!(tl.head_cursor(&pa).await.unwrap(), 2);
    assert_eq!(tl.head_cursor(&pb).await.unwrap(), 1, "proj-b cursor should start from 1");
}

#[tokio::test]
async fn signature_verification() {
    let (tl, pid, _db) = setup().await;

    tl.append(&pid, "site-a", "agent", EntryType::Focus, "important task", None).await.unwrap();

    let entries = tl.since(&pid, 0).await.unwrap();
    let entry   = &entries[0];

    assert!(entry.signature.is_some(), "entries must carry HMAC signature");
    assert!(tl.verify_entry(entry), "signature should verify against correct secret");
}

#[tokio::test]
async fn wrong_secret_fails_verification() {
    let (tl, pid, db) = setup().await;

    tl.append(&pid, "site-a", "agent", EntryType::Focus, "data", None).await.unwrap();
    let entries = tl.since(&pid, 0).await.unwrap();

    let tl_wrong = TaskLog::new(db, b"wrong-secret");
    assert!(!tl_wrong.verify_entry(&entries[0]), "wrong secret should fail verification");
}

#[tokio::test]
async fn empty_project_returns_empty_list() {
    let (tl, _pid, _db) = setup().await;
    // Use a separate project that has no entries
    let db2 = db::open(":memory:").await.unwrap();
    let tl2 = TaskLog::new(db2.clone(), b"secret");
    let projects2 = ProjectStore::new(db2.clone());
    let pid2 = projects2.create("empty-proj", "https://z.com", "", "site-a", "", "").await.unwrap().id;
    let entries = tl2.since(&pid2, 0).await.unwrap();
    assert!(entries.is_empty());
}

#[tokio::test]
async fn entry_types_stored_correctly() {
    let (tl, pid, _db) = setup().await;

    tl.append(&pid, "gw", "agent", EntryType::Focus,   "f", None).await.unwrap();
    tl.append(&pid, "gw", "agent", EntryType::Note,    "n", None).await.unwrap();
    tl.append(&pid, "gw", "agent", EntryType::Done,    "d", None).await.unwrap();
    tl.append(&pid, "gw", "agent", EntryType::Blocked, "b", None).await.unwrap();

    let entries = tl.since(&pid, 0).await.unwrap();
    let types: Vec<&str> = entries.iter().map(|e| e.entry_type.as_str()).collect();
    assert!(types.contains(&"focus"));
    assert!(types.contains(&"note"));
    assert!(types.contains(&"done"));
    assert!(types.contains(&"blocked"));
}

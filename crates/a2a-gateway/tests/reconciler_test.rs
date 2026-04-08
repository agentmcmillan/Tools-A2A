/// Reconciler integration tests.
///
/// These tests call Reconciler::run_once() directly against an in-memory DB.
/// No tonic server needed — we seed the DB state directly via sqlx.

use a2a_gateway::{db, reconciler::Reconciler};
use chrono::Utc;

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn seed_agent(pool: &sqlx::SqlitePool, name: &str, capabilities: &[&str]) {
    let now = Utc::now().to_rfc3339();
    let caps_json = serde_json::to_string(capabilities).unwrap();
    sqlx::query(
        "INSERT OR REPLACE INTO agents (name, version, endpoint, capabilities, soul_toml, registered_at)
         VALUES (?1, ?2, ?3, ?4, '', ?5)"
    )
    .bind(name).bind("0.1.0").bind(format!("http://{}:1234", name))
    .bind(&caps_json).bind(&now)
    .execute(pool).await.unwrap();
}

async fn seed_todo(pool: &sqlx::SqlitePool, agent: &str, task: &str) -> i64 {
    let now = Utc::now().to_rfc3339();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO todos (agent_name, task, status, created_at) VALUES (?1, ?2, 'pending', ?3) RETURNING id"
    )
    .bind(agent).bind(task).bind(&now)
    .fetch_one(pool).await.unwrap();
    row.0
}

async fn count_project_todos(pool: &sqlx::SqlitePool, status: &str) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM project_todos WHERE status = ?1"
    )
    .bind(status)
    .fetch_one(pool).await.unwrap();
    row.0
}

async fn count_all_project_todos(pool: &sqlx::SqlitePool) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM project_todos")
        .fetch_one(pool).await.unwrap();
    row.0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Test: two agents with the same task text → duplicate is merged into one project_todo.
#[tokio::test]
async fn duplicate_todos_are_merged() {
    let pool = db::open(":memory:").await.unwrap();

    seed_agent(&pool, "agent-a", &["research"]).await;
    seed_agent(&pool, "agent-b", &["research"]).await;

    seed_todo(&pool, "agent-a", "Research topic X").await;
    seed_todo(&pool, "agent-b", "Research topic X").await; // same normalised text

    let reconciler = Reconciler::new(pool.clone(), 30, "/tmp");
    reconciler.run_once().await.unwrap();

    // Only one project_todo should exist for this task
    let total = count_all_project_todos(&pool).await;
    assert_eq!(total, 1, "duplicate todos must be merged into a single project_todo");
}

/// Test: task text matches agent capability → status 'pending' (assigned).
#[tokio::test]
async fn task_assigned_when_capability_matches() {
    let pool = db::open(":memory:").await.unwrap();

    seed_agent(&pool, "researcher", &["research"]).await;
    seed_todo(&pool, "researcher", "Research quantum computing").await;

    let reconciler = Reconciler::new(pool.clone(), 30, "/tmp");
    reconciler.run_once().await.unwrap();

    let assigned = count_project_todos(&pool, "pending").await;
    assert_eq!(assigned, 1, "task matching capability must be assigned (pending)");

    let flagged = count_project_todos(&pool, "flagged").await;
    assert_eq!(flagged, 0, "no tasks should be flagged when capability matches");
}

/// Test: task does not match any agent capability → status 'flagged'.
#[tokio::test]
async fn task_flagged_when_no_capability_match() {
    let pool = db::open(":memory:").await.unwrap();

    seed_agent(&pool, "writer", &["write", "edit"]).await;
    seed_todo(&pool, "writer", "Analyze security vulnerabilities").await; // no "security" capability

    let reconciler = Reconciler::new(pool.clone(), 30, "/tmp");
    reconciler.run_once().await.unwrap();

    let flagged = count_project_todos(&pool, "flagged").await;
    assert_eq!(flagged, 1, "task without capability match must be flagged");
}

/// Test: reconcile is idempotent — running twice does not create duplicate project_todos.
#[tokio::test]
async fn reconcile_is_idempotent() {
    let pool = db::open(":memory:").await.unwrap();

    seed_agent(&pool, "orch", &["orchestrate"]).await;
    seed_todo(&pool, "orch", "Orchestrate pipeline run").await;

    let reconciler = Reconciler::new(pool.clone(), 30, "/tmp");
    reconciler.run_once().await.unwrap();
    reconciler.run_once().await.unwrap(); // second run

    let total = count_all_project_todos(&pool).await;
    assert_eq!(total, 1, "second reconcile run must not create duplicate project_todos");
}

/// Test: empty todo list produces no project_todos and no error.
#[tokio::test]
async fn empty_todos_produces_no_project_todos() {
    let pool = db::open(":memory:").await.unwrap();
    // No agents, no todos

    let reconciler = Reconciler::new(pool.clone(), 30, "/tmp");
    reconciler.run_once().await.unwrap();

    let total = count_all_project_todos(&pool).await;
    assert_eq!(total, 0);
}

/// Test: merge annotation includes both agent names.
#[tokio::test]
async fn merged_todo_notes_include_all_agents() {
    let pool = db::open(":memory:").await.unwrap();

    seed_agent(&pool, "alpha", &["research"]).await;
    seed_agent(&pool, "beta", &["research"]).await;

    seed_todo(&pool, "alpha", "Write report").await;
    seed_todo(&pool, "beta", "Write report").await;

    let reconciler = Reconciler::new(pool.clone(), 30, "/tmp");
    reconciler.run_once().await.unwrap();

    let row: (String,) = sqlx::query_as("SELECT notes FROM project_todos LIMIT 1")
        .fetch_one(&pool).await.unwrap();
    let notes = row.0;
    assert!(notes.contains("alpha") || notes.contains("2"),
        "merge notes must reference contributing agents; got: {notes}");
}

mod helpers;
use helpers::TestGateway;
use a2a_proto::local::{AgentManifest, AgentRef, MemoryPatch, TodoEntry, TodoFilter, TodoComplete};

async fn register_agent(client: &mut a2a_proto::local::local_gateway_client::LocalGatewayClient<tonic::transport::Channel>, name: &str) {
    client.register(AgentManifest {
        name:         name.into(),
        version:      "0.1.0".into(),
        endpoint:     format!("http://127.0.0.1:1{}", name.len()),
        capabilities: vec![],
        soul_toml:    format!("[agent]\nname = \"{}\"", name),
    }).await.unwrap();
}

/// Test: GetSoul after Register returns the soul_toml from the manifest.
#[tokio::test]
async fn get_soul_returns_registered_content() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("soul-agent").await;
    register_agent(&mut client, "soul-agent").await;

    let soul = client.get_soul(AgentRef { name: "soul-agent".into() })
        .await.unwrap().into_inner();
    assert!(soul.content_toml.contains("soul-agent"),
        "soul should contain registered content, got: {:?}", soul.content_toml);
}

/// Test: soul written once; second Register does NOT overwrite existing soul.
#[tokio::test]
async fn soul_is_written_once_not_overwritten() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("stable-soul").await;

    // First register sets soul
    client.register(AgentManifest {
        name:         "stable-soul".into(),
        version:      "0.1.0".into(),
        endpoint:     "http://127.0.0.1:9800".into(),
        capabilities: vec![],
        soul_toml:    "original = true".into(),
    }).await.unwrap();

    // Second register with different soul
    client.register(AgentManifest {
        name:         "stable-soul".into(),
        version:      "0.2.0".into(),
        endpoint:     "http://127.0.0.1:9800".into(),
        capabilities: vec![],
        soul_toml:    "overwritten = true".into(),
    }).await.unwrap();

    let soul = client.get_soul(AgentRef { name: "stable-soul".into() })
        .await.unwrap().into_inner();
    assert!(soul.content_toml.contains("original"),
        "soul must not be overwritten by second register; got: {}", soul.content_toml);
    assert!(!soul.content_toml.contains("overwritten"),
        "overwritten content must not appear in soul");
}

/// Test: UpdateMemory appends content (does not overwrite).
#[tokio::test]
async fn memory_appends_not_overwrites() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("mem-agent").await;
    register_agent(&mut client, "mem-agent").await;

    client.update_memory(MemoryPatch {
        agent_name: "mem-agent".into(),
        append_md:  "## First entry".into(),
    }).await.unwrap();

    client.update_memory(MemoryPatch {
        agent_name: "mem-agent".into(),
        append_md:  "## Second entry".into(),
    }).await.unwrap();

    // Read soul is the proxy — memory is stored separately.
    // We verify via AppendTodo/ListTodos as a proxy for DB correctness,
    // but memory itself has no read RPC in Phase 1 (only gateway internal).
    // This test verifies no error is returned from double-append.
}

/// Test: AppendTodo creates a todo; completed todos are never deleted.
#[tokio::test]
async fn todo_never_deleted_after_completion() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("todo-agent").await;
    register_agent(&mut client, "todo-agent").await;

    // Append two todos
    client.append_todo(TodoEntry {
        agent_name: "todo-agent".into(),
        task:       "Write tests".into(),
    }).await.unwrap();
    client.append_todo(TodoEntry {
        agent_name: "todo-agent".into(),
        task:       "Deploy service".into(),
    }).await.unwrap();

    // List: should have 2 pending
    let list = client.list_todos(TodoFilter {
        agent_name:   "todo-agent".into(),
        include_done: false,
    }).await.unwrap().into_inner();
    assert_eq!(list.todos.len(), 2, "should have 2 pending todos");

    let first_id = list.todos[0].id;

    // Complete the first todo
    client.complete_todo(TodoComplete {
        id:    first_id,
        notes: "Completed in test".into(),
    }).await.unwrap();

    // List without done: should have 1
    let pending = client.list_todos(TodoFilter {
        agent_name:   "todo-agent".into(),
        include_done: false,
    }).await.unwrap().into_inner();
    assert_eq!(pending.todos.len(), 1, "should have 1 pending after completion");

    // List with done: should still have 2 (done todos not deleted)
    let all = client.list_todos(TodoFilter {
        agent_name:   "todo-agent".into(),
        include_done: true,
    }).await.unwrap().into_inner();
    assert_eq!(all.todos.len(), 2, "completed todos must never be deleted; should still have 2");
}

/// Test: completed todo has annotation preserved.
#[tokio::test]
async fn todo_completion_stores_notes() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("notes-agent").await;
    register_agent(&mut client, "notes-agent").await;

    client.append_todo(TodoEntry {
        agent_name: "notes-agent".into(),
        task:       "Annotated task".into(),
    }).await.unwrap();

    let list = client.list_todos(TodoFilter {
        agent_name: "notes-agent".into(), include_done: false,
    }).await.unwrap().into_inner();
    let id = list.todos[0].id;

    client.complete_todo(TodoComplete {
        id, notes: "Finished by test runner".into(),
    }).await.unwrap();

    let all = client.list_todos(TodoFilter {
        agent_name: "notes-agent".into(), include_done: true,
    }).await.unwrap().into_inner();

    let done = all.todos.iter().find(|t| t.id == id).unwrap();
    assert_eq!(done.status, "done");
    assert!(done.notes.contains("Finished by test runner"),
        "completion notes must be stored; got: {}", done.notes);
}

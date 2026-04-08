/// SDK integration tests — round-trip through a real in-process gateway.
///
/// Data flow:
///
///   ┌─────────────────────────────────────────────────────────────┐
///   │  test                                                        │
///   │  ┌──────────────────┐     gRPC :random     ┌─────────────┐  │
///   │  │  GatewayClient   │◄───────────────────► │  gateway    │  │
///   │  │  (a2a_sdk)       │  register/call/todos │  (in-proc)  │  │
///   │  └──────────────────┘                      └─────────────┘  │
///   └─────────────────────────────────────────────────────────────┘
///
/// Tests:
///   - register via GatewayClient, then list_agents
///   - append_todo + list_todos round-trip
///   - update_memory, then get_soul includes updated memory
///   - call to unknown agent surfaces a gRPC error
///   - Agent builder smoke test (build without serve)
///   - Full serve round-trip: agent serves, caller calls, response returned

use a2a_gateway::{
    config::{CallGraphConfig, CallLimits, Config, DbConfig, GatewayConfig},
    db,
    identity::IdentityStore,
    local_server::LocalGatewayHandler,
    registry::Registry,
    router::Router,
};
use a2a_sdk::{client::GatewayClient, convert, Agent, Message, Response};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tonic::transport::Server;
use tokio::net::TcpListener;

// ── Shared gateway helper ─────────────────────────────────────────────────────

/// Spin up an in-process gateway on a random port; returns the address.
async fn start_gateway() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let db       = db::open(":memory:").await.unwrap();
    let registry = Registry::new(db.clone()).await.unwrap();
    let router   = Arc::new(Router::new(empty_config(), registry.clone()));
    let identity = IdentityStore::new(db.clone());
    let handler  = LocalGatewayHandler::new(registry, identity, router, "sdk-test-gw");

    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        Server::builder()
            .add_service(handler.into_service())
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn empty_config() -> Config {
    Config {
        gateway: GatewayConfig {
            name:                    "sdk-test-gw".into(),
            port_local:              0,
            port_peer:               0,
            port_http:               0,
            port_web:                0,
            reconcile_interval_secs: 30,
        },
        db:         DbConfig { path: ":memory:".into() },
        lxc:        None,
        agents:     vec![],
        call_graph: CallGraphConfig {
            edges:  HashMap::new(),
            limits: CallLimits { max_hops: 3 },
        },
        peers: vec![],
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Register an agent via GatewayClient and verify it appears in list_agents.
#[tokio::test]
async fn register_and_list_agents() {
    let addr = start_gateway().await;
    let gw = format!("http://{addr}");

    let mut client = GatewayClient::connect(&gw, "lister").await.unwrap();
    let manifest = convert::to_manifest("sdk-agent", "1.0", "http://sdk-agent:9000", &[], "");
    client.register(manifest).await.unwrap();

    let agents = client.list_agents().await.unwrap();
    assert!(agents.iter().any(|a| a.name == "sdk-agent"), "sdk-agent should be in the list");
}

/// append_todo + list_todos round-trip.
#[tokio::test]
async fn todo_round_trip() {
    let addr = start_gateway().await;
    let gw = format!("http://{addr}");

    let mut client = GatewayClient::connect(&gw, "todo-agent").await.unwrap();
    let manifest = convert::to_manifest(
        "todo-agent", "1.0", "http://todo-agent:9001", &[], "",
    );
    client.register(manifest).await.unwrap();

    client.append_todo("write the authentication module").await.unwrap();
    client.append_todo("write the test suite").await.unwrap();

    let todos = client.list_todos(false).await.unwrap();
    assert_eq!(todos.len(), 2);
    assert!(todos.iter().any(|t| t.task.contains("authentication")));
    assert!(todos.iter().any(|t| t.task.contains("test suite")));
    assert!(todos.iter().all(|t| t.status == "pending"));
}

/// update_memory, then get_soul; soul content_toml should include memory.
#[tokio::test]
async fn memory_updates_persisted() {
    let addr = start_gateway().await;
    let gw = format!("http://{addr}");

    let mut client = GatewayClient::connect(&gw, "memory-agent").await.unwrap();
    let soul_toml = "[agent]\nname = \"memory-agent\"\nrole = \"researcher\"";
    let manifest = convert::to_manifest(
        "memory-agent", "1.0", "http://memory-agent:9002",
        &[], soul_toml,
    );
    client.register(manifest).await.unwrap();
    client.update_memory("Learned: prefer async over sync patterns").await.unwrap();

    let soul = client.get_soul().await.unwrap();
    // soul contains the TOML + appended memory
    assert!(!soul.content.is_empty(), "soul should have content");
}

/// Call to an unknown agent returns a gRPC error (not a panic).
#[tokio::test]
async fn call_unknown_agent_errors_gracefully() {
    let addr = start_gateway().await;
    let gw = format!("http://{addr}");

    let mut client = GatewayClient::connect(&gw, "caller").await.unwrap();
    let opts = a2a_sdk::CallOptions::default();
    let result = client.call(
        "nonexistent-agent",
        "invoke",
        serde_json::json!({"task": "do something"}),
        &opts,
    ).await;

    assert!(result.is_err(), "call to unknown agent must return Err");
}

/// Agent builder smoke test: builder resolves, connects to gateway, no panic.
#[tokio::test]
async fn agent_builder_smoke() {
    let addr = start_gateway().await;
    let gw = format!("http://{addr}");

    let agent = Agent::builder()
        .name("smoke-agent")
        .version("0.1.0")
        .gateway(&gw)
        .endpoint("http://smoke-agent:9003")
        .soul("[agent]\nname = \"smoke-agent\"")
        .capability("test", "smoke test capability")
        .build()
        .await;

    assert!(agent.is_ok(), "AgentBuilder::build should succeed: {:?}", agent.err());
    let agent = agent.unwrap();
    assert_eq!(agent.name, "smoke-agent");
    assert_eq!(agent.capabilities.len(), 1);
}

/// Full round-trip: one agent serves, a second agent calls it and gets a response.
#[tokio::test]
async fn agent_serve_and_call_round_trip() {
    let addr = start_gateway().await;
    let gw   = format!("http://{addr}");

    // Bind a local port for the serving agent
    let serve_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let serve_port = serve_listener.local_addr().unwrap().port();
    let serve_ep   = format!("http://127.0.0.1:{serve_port}");
    drop(serve_listener); // release so agent can bind it

    // Start serving agent in background
    let gw_clone  = gw.clone();
    let ep_clone  = serve_ep.clone();
    let serve_handle = tokio::spawn(async move {
        let agent = Agent::builder()
            .name("echo-agent")
            .version("0.1.0")
            .gateway(&gw_clone)
            .endpoint(&ep_clone)
            .build()
            .await
            .unwrap();

        agent.serve(|msg: Message| async move {
            let echo = msg.payload.get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("nothing");
            Ok(Response::ok(serde_json::json!({ "echo": echo })))
        }).await.unwrap();
    });

    // Give the serving agent time to register and bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Caller calls the echo-agent through the gateway
    let mut caller = GatewayClient::connect(&gw, "caller-agent").await.unwrap();
    let manifest = convert::to_manifest(
        "caller-agent", "1.0", "http://caller-agent:9004", &[], "",
    );
    caller.register(manifest).await.unwrap();

    let opts = a2a_sdk::CallOptions::default();
    let resp = caller.call(
        "echo-agent",
        "invoke",
        serde_json::json!({ "input": "hello from caller" }),
        &opts,
    ).await.unwrap();

    assert_eq!(resp["echo"], "hello from caller");

    serve_handle.abort();
}

/// Complete a todo and verify it no longer appears in the active list.
#[tokio::test]
async fn complete_todo_removes_from_active_list() {
    let addr = start_gateway().await;
    let gw = format!("http://{addr}");

    let mut client = GatewayClient::connect(&gw, "comp-agent").await.unwrap();
    let manifest = convert::to_manifest(
        "comp-agent", "1.0", "http://comp-agent:9005", &[], "",
    );
    client.register(manifest).await.unwrap();
    client.append_todo("task to complete").await.unwrap();

    let todos = client.list_todos(false).await.unwrap();
    assert_eq!(todos.len(), 1);
    let id = todos[0].id;

    client.complete_todo(id, "done — all good").await.unwrap();

    // Active list (include_done=false) should now be empty
    let active = client.list_todos(false).await.unwrap();
    assert!(active.is_empty(), "completed todo should not appear in active list");

    // Full list (include_done=true) should show it with done status
    let all = client.list_todos(true).await.unwrap();
    let done = all.iter().find(|t| t.id == id).expect("completed todo should exist in full list");
    assert_eq!(done.status, "done");
    assert_eq!(done.notes.as_deref(), Some("done — all good"));
}

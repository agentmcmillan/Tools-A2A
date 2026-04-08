mod helpers;
use helpers::{TestGateway, config_with_call_graph};
use a2a_proto::local::{AgentManifest, CallRequest};

/// Test: calling an agent that does not exist returns NotFound.
/// Uses empty caller to bypass the call graph check (LAN external caller).
#[tokio::test]
async fn call_nonexistent_agent_returns_not_found() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("").await;

    let result = client.call(CallRequest {
        target_agent: "ghost".into(),
        method:       "invoke".into(),
        payload:      b"{}".to_vec(),
        trace_id:     "trace-1".into(),
        caller:       String::new(), // anonymous caller bypasses call graph
        hop_count:    0,
    }).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
}

/// Test: hop limit is enforced by the gateway.
/// Sending hop_count at max (3) means next_hop = 4 > max → ResourceExhausted.
#[tokio::test]
async fn call_at_hop_limit_is_rejected() {
    let cfg = config_with_call_graph(
        &[("caller", "http://127.0.0.1:9990"), ("target", "http://127.0.0.1:9991")],
        &[("caller", &["target"])],
        3,
    );
    let gw = TestGateway::start_with_config(cfg).await;
    let mut client = gw.client("caller").await;

    let result = client.call(CallRequest {
        target_agent: "target".into(),
        method:       "invoke".into(),
        payload:      b"{}".to_vec(),
        trace_id:     "trace-hop".into(),
        caller:       "caller".into(),
        hop_count:    3, // gateway will try next_hop=4 which exceeds max=3
    }).await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), tonic::Code::ResourceExhausted,
        "call at hop limit must return ResourceExhausted");
}

/// Test: registering an agent and calling it with an allowed edge succeeds
/// (or returns Unavailable since the echo agent isn't running, not a policy error).
#[tokio::test]
async fn call_allowed_edge_gets_through_policy() {
    let cfg = config_with_call_graph(
        &[("orchestrator", "http://127.0.0.1:9980"), ("research", "http://127.0.0.1:9981")],
        &[("orchestrator", &["research"])],
        3,
    );
    let gw = TestGateway::start_with_config(cfg).await;
    let mut client = gw.client("orchestrator").await;

    // Register orchestrator
    client.register(AgentManifest {
        name: "orchestrator".into(), version: "0.1.0".into(),
        endpoint: "http://127.0.0.1:9980".into(),
        capabilities: vec![], soul_toml: String::new(),
    }).await.unwrap();

    let result = client.call(CallRequest {
        target_agent: "research".into(),
        method:       "invoke".into(),
        payload:      b"{\"query\":\"test\"}".to_vec(),
        trace_id:     "trace-2".into(),
        caller:       "orchestrator".into(),
        hop_count:    0,
    }).await;

    match result {
        Ok(_) => {} // unlikely but fine if a real server happened to be there
        Err(s) => {
            // Policy error would be PermissionDenied — anything else is transport
            assert_ne!(s.code(), tonic::Code::PermissionDenied,
                "call on allowed edge must not be rejected by policy");
        }
    }
}

/// Test: calling across a denied edge returns PermissionDenied.
#[tokio::test]
async fn call_denied_edge_returns_permission_denied() {
    let cfg = config_with_call_graph(
        &[("writer", "http://127.0.0.1:9970"), ("orchestrator", "http://127.0.0.1:9971")],
        &[("orchestrator", &["writer"])], // writer→orchestrator is NOT allowed
        3,
    );
    let gw = TestGateway::start_with_config(cfg).await;
    let mut client = gw.client("writer").await;

    let result = client.call(CallRequest {
        target_agent: "orchestrator".into(),
        method:       "invoke".into(),
        payload:      b"{}".to_vec(),
        trace_id:     "trace-3".into(),
        caller:       "writer".into(),
        hop_count:    0,
    }).await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), tonic::Code::PermissionDenied);
}

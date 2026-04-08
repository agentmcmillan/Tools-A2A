mod helpers;
use helpers::TestGateway;
use a2a_proto::local::{AgentManifest, Empty};

/// Test: a fresh Register upserts the agent and echoes the gateway name back.
#[tokio::test]
async fn register_returns_ack() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("agent-a").await;

    let ack = client.register(AgentManifest {
        name:         "agent-a".into(),
        version:      "0.1.0".into(),
        endpoint:     "http://127.0.0.1:9999".into(),
        capabilities: vec!["research".into()],
        soul_toml:    "[agent]\nrole=\"researcher\"".into(),
    }).await.unwrap().into_inner();

    assert_eq!(ack.agent_id, "agent-a");
    assert_eq!(ack.gateway, "test-gw");
}

/// Test: registering the same agent twice does not create a duplicate; ListAgents returns 1.
#[tokio::test]
async fn duplicate_register_is_upserted() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("agent-b").await;

    let manifest = AgentManifest {
        name:         "agent-b".into(),
        version:      "0.1.0".into(),
        endpoint:     "http://127.0.0.1:9998".into(),
        capabilities: vec![],
        soul_toml:    String::new(),
    };
    client.register(manifest.clone()).await.unwrap();
    client.register(manifest.clone()).await.unwrap();

    let list = client.list_agents(Empty {}).await.unwrap().into_inner();
    let matches: Vec<_> = list.agents.iter().filter(|a| a.name == "agent-b").collect();
    assert_eq!(matches.len(), 1, "expected exactly one agent-b after duplicate register");
}

/// Test: registering an agent with an empty name returns an error.
#[tokio::test]
async fn register_empty_name_rejected() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("").await;

    let result = client.register(AgentManifest {
        name:         String::new(),
        version:      "0.1.0".into(),
        endpoint:     "http://127.0.0.1:9997".into(),
        capabilities: vec![],
        soul_toml:    String::new(),
    }).await;

    assert!(result.is_err(), "empty name should be rejected");
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

/// Test: registering with an empty endpoint is rejected.
#[tokio::test]
async fn register_empty_endpoint_rejected() {
    let gw = TestGateway::start().await;
    let mut client = gw.client("agent-c").await;

    let result = client.register(AgentManifest {
        name:         "agent-c".into(),
        version:      "0.1.0".into(),
        endpoint:     String::new(),
        capabilities: vec![],
        soul_toml:    String::new(),
    }).await;

    assert!(result.is_err(), "empty endpoint should be rejected");
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

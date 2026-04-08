/// TestGateway — in-process gateway on a random port.
///
/// Spins up a real tonic server with a fresh SQLite :memory: DB.
/// Each test gets an isolated instance; no filesystem side-effects.
///
/// Usage:
/// ```
/// let gw = TestGateway::start().await;
/// let mut client = gw.client("my-agent").await;
/// ```

use a2a_proto::local::local_gateway_client::LocalGatewayClient;
use a2a_gateway::{
    config::{
        CallGraphConfig, CallLimits, Config,
        DbConfig, GatewayConfig, SeedAgent,
    },
    db,
    identity::IdentityStore,
    local_server::LocalGatewayHandler,
    registry::Registry,
    router::Router,
};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tonic::transport::{Channel, Server};
use tokio::net::TcpListener;

pub struct TestGateway {
    pub addr: SocketAddr,
    pub cfg:  Config,
}

impl TestGateway {
    pub async fn start() -> Self {
        Self::start_with_config(default_config()).await
    }

    pub async fn start_with_config(cfg: Config) -> Self {
        // Bind to OS-assigned port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let db = db::open(":memory:").await.unwrap();
        let registry = Registry::new(db.clone()).await.unwrap();

        // Seed agents from config
        for seed in &cfg.agents {
            use chrono::Utc;
            use a2a_gateway::registry::AgentEntry;
            registry.upsert(AgentEntry {
                name:          seed.name.clone(),
                version:       "test".into(),
                endpoint:      seed.endpoint.clone(),
                capabilities:  vec![],
                soul_toml:     String::new(),
                last_seen:     None,
                registered_at: Utc::now().to_rfc3339(),
            }).await.unwrap();
        }

        let router   = Arc::new(Router::new(cfg.clone(), registry.clone()));
        let identity = IdentityStore::new(db.clone());
        let handler  = LocalGatewayHandler::new(registry.clone(), identity, router, &cfg.gateway.name);

        // Don't start reconciler in tests — use run_once() directly
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        tokio::spawn(async move {
            Server::builder()
                .add_service(handler.into_service())
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        // Brief yield so the server goroutine starts accepting
        tokio::task::yield_now().await;

        TestGateway { addr, cfg }
    }

    pub async fn client(&self, agent_name: &str) -> LocalGatewayClient<Channel> {
        let endpoint = format!("http://{}", self.addr);
        let channel = Channel::from_shared(endpoint).unwrap().connect().await.unwrap();
        // agent_name isn't sent to tonic — caller sets it in CallRequest.caller explicitly.
        let _ = agent_name;
        LocalGatewayClient::new(channel)
    }
}

pub fn default_config() -> Config {
    Config {
        gateway: GatewayConfig {
            name:                    "test-gw".into(),
            port_local:              7240,
            port_peer:               7241,
            port_http:               7242,
            port_web:                7243,
            reconcile_interval_secs: 30,
        },
        db: DbConfig { path: ":memory:".into() },
        lxc: None,
        agents: vec![],
        call_graph: CallGraphConfig {
            edges:  HashMap::new(),
            limits: CallLimits { max_hops: 3 },
        },
        peers: vec![],
    }
}

pub fn config_with_agents(agents: &[(&str, &str)]) -> Config {
    let mut cfg = default_config();
    cfg.agents = agents.iter().map(|(name, ep)| SeedAgent {
        name:     name.to_string(),
        endpoint: ep.to_string(),
    }).collect();
    cfg
}

pub fn config_with_call_graph(
    agents: &[(&str, &str)],
    edges: &[(&str, &[&str])],
    max_hops: u32,
) -> Config {
    let mut cfg = config_with_agents(agents);
    cfg.call_graph.limits.max_hops = max_hops;
    for (caller, targets) in edges {
        cfg.call_graph.edges.insert(
            caller.to_string(),
            targets.iter().map(|s| s.to_string()).collect(),
        );
    }
    cfg
}

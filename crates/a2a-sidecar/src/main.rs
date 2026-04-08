/// a2a-sidecar — transparent Unix socket → TCP gRPC proxy.
///
/// Lets non-Rust agents (Python, JS, etc.) connect over a local Unix domain
/// socket and speak the LocalGateway proto without needing to implement TLS
/// or manage TCP connections to the gateway directly.
///
/// Wire diagram:
///
///   ┌─────────────────────┐   gRPC/Unix      ┌──────────────────────────────┐
///   │  Python/JS agent     │ ────────────────► │  a2a-sidecar                  │
///   │  (any language)      │   /run/a2a/*.sock │  LocalGateway server          │
///   └─────────────────────┘                   │  (proxies every RPC)          │
///                                             │  ─────────────────────────── │
///                                             │  gateway client               │
///                                             └──────────────┬───────────────┘
///                                                            │ gRPC/TCP
///                                                   http://127.0.0.1:7240
///                                                            │
///                                             ┌──────────────▼───────────────┐
///                                             │  a2a-gateway (real gateway)  │
///                                             └──────────────────────────────┘
///
/// Configuration (env vars):
///   A2A_SOCKET   — Unix socket path (default: /run/a2a/agent.sock)
///   A2A_GATEWAY  — upstream gateway TCP address (default: http://127.0.0.1:7240)
///
/// The sidecar is stateless: it forwards every RPC call and response verbatim.
/// Authentication, call graphs, and hop limits are enforced by the real gateway.
///
/// Non-streaming RPCs are forwarded synchronously.
/// The `Stream` (server-streaming) RPC is proxied with an async channel bridge.

use a2a_proto::local::{
    local_gateway_client::LocalGatewayClient,
    local_gateway_server::{LocalGateway, LocalGatewayServer},
    Ack, AgentList, AgentManifest, AgentRef, CallRequest, CallResponse,
    Empty, HealthStatus, MemoryPatch, RegisterAck, SoulContent,
    StreamChunk, TodoComplete, TodoEntry, TodoFilter, TodoList,
};
use anyhow::{Context, Result};
use futures::StreamExt;
use std::pin::Pin;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{transport::Channel, Request, Response, Status};
use tracing::info;

// ── Proxy service ─────────────────────────────────────────────────────────────

/// Transparent proxy: every method forwards to the upstream gateway.
struct ProxyService {
    upstream: LocalGatewayClient<Channel>,
}

impl ProxyService {
    async fn connect(gateway_addr: &str) -> Result<Self> {
        let channel = Channel::from_shared(gateway_addr.to_owned())
            .context("invalid gateway address")?
            .connect()
            .await
            .with_context(|| format!("connecting to gateway at {gateway_addr}"))?;
        Ok(Self { upstream: LocalGatewayClient::new(channel) })
    }
}

type ServiceStream<T> = Pin<Box<dyn futures::Stream<Item = std::result::Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl LocalGateway for ProxyService {
    // ── Registration ─────────────────────────────────────────────────────────

    async fn register(
        &self, req: Request<AgentManifest>,
    ) -> std::result::Result<Response<RegisterAck>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.register(req.into_inner()).await
    }

    async fn heartbeat(
        &self, req: Request<AgentRef>,
    ) -> std::result::Result<Response<Ack>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.heartbeat(req.into_inner()).await
    }

    // ── Call / Stream ─────────────────────────────────────────────────────────

    async fn call(
        &self, req: Request<CallRequest>,
    ) -> std::result::Result<Response<CallResponse>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.call(req.into_inner()).await
    }

    type StreamStream = ServiceStream<StreamChunk>;

    async fn stream(
        &self, req: Request<CallRequest>,
    ) -> std::result::Result<Response<Self::StreamStream>, Status> {
        let mut upstream = self.upstream.clone();
        let inner = req.into_inner();

        let response = upstream.stream(inner).await?;
        let mut incoming = response.into_inner();

        // Bridge the upstream streaming response to the downstream client
        let outgoing = async_stream::stream! {
            while let Some(item) = incoming.next().await {
                yield item;
            }
        };

        Ok(Response::new(Box::pin(outgoing)))
    }

    // ── Agent list / health ───────────────────────────────────────────────────

    async fn list_agents(
        &self, req: Request<Empty>,
    ) -> std::result::Result<Response<AgentList>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.list_agents(req.into_inner()).await
    }

    async fn health(
        &self, req: Request<Empty>,
    ) -> std::result::Result<Response<HealthStatus>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.health(req.into_inner()).await
    }

    // ── Identity ──────────────────────────────────────────────────────────────

    async fn get_soul(
        &self, req: Request<AgentRef>,
    ) -> std::result::Result<Response<SoulContent>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.get_soul(req.into_inner()).await
    }

    async fn update_memory(
        &self, req: Request<MemoryPatch>,
    ) -> std::result::Result<Response<Ack>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.update_memory(req.into_inner()).await
    }

    async fn append_todo(
        &self, req: Request<TodoEntry>,
    ) -> std::result::Result<Response<Ack>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.append_todo(req.into_inner()).await
    }

    async fn list_todos(
        &self, req: Request<TodoFilter>,
    ) -> std::result::Result<Response<TodoList>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.list_todos(req.into_inner()).await
    }

    async fn complete_todo(
        &self, req: Request<TodoComplete>,
    ) -> std::result::Result<Response<Ack>, Status> {
        let mut upstream = self.upstream.clone();
        upstream.complete_todo(req.into_inner()).await
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("a2a_sidecar=info".parse()?)
            .add_directive("info".parse()?))
        .init();

    let socket_path = std::env::var("A2A_SOCKET")
        .unwrap_or_else(|_| "/run/a2a/agent.sock".into());
    let gateway_addr = std::env::var("A2A_GATEWAY")
        .unwrap_or_else(|_| "http://127.0.0.1:7240".into());

    info!(socket = %socket_path, gateway = %gateway_addr, "a2a-sidecar starting");

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&socket_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating socket directory {}", parent.display()))?;
    }

    // Remove stale socket from previous run
    let _ = std::fs::remove_file(&socket_path);

    let proxy = ProxyService::connect(&gateway_addr).await?;
    info!("connected to upstream gateway at {gateway_addr}");

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding Unix socket at {socket_path}"))?;
    let incoming = UnixListenerStream::new(listener);
    info!(socket = %socket_path, "sidecar listening");

    tonic::transport::Server::builder()
        .add_service(LocalGatewayServer::new(proxy))
        .serve_with_incoming(incoming)
        .await
        .context("sidecar server error")?;

    Ok(())
}

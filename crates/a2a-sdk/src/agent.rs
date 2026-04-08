/// High-level Agent API for SDK users.
///
/// ```rust,no_run
/// use a2a_sdk::{Agent, Message, Response};
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let agent = Agent::builder()
///         .name("research")
///         .version("0.1.0")
///         .gateway("http://127.0.0.1:7240")
///         .soul("[agent]\nrole = \"researcher\"")
///         .capability("research", "Web and document research")
///         .build()
///         .await?;
///
///     agent.serve(|msg: Message| async move {
///         Ok(Response::ok(serde_json::json!({ "result": "done" })))
///     }).await
/// }
/// ```

use crate::{
    client::GatewayClient,
    convert,
    hooks::{self, HookRegistry, TaskAssignedEvent, SnapshotChangedEvent, LifecycleEvent, BroadcastEvent},
    options::CallOptions,
    types::{Capability, Message, Response, Soul, StreamChunk, TodoItem},
};
use a2a_proto::local::{
    local_gateway_server::{LocalGateway, LocalGatewayServer},
    Ack, AgentList, AgentManifest, AgentRef, CallRequest, CallResponse,
    Empty, HealthStatus, MemoryPatch, RegisterAck, SoulContent,
    StreamChunk as ProtoStreamChunk,
    TodoComplete, TodoEntry, TodoFilter, TodoList,
};
use anyhow::Result;
use futures::Stream;
use std::{net::SocketAddr, pin::Pin, sync::Arc};
use tokio::sync::RwLock;
use tonic::{transport::Server, Request, Response as TonicResponse, Status};
use tracing::info;

// ── Builder ───────────────────────────────────────────────────────────────────

pub struct AgentBuilder {
    name:         Option<String>,
    version:      String,
    gateway:      Option<String>,
    soul_toml:    String,
    capabilities: Vec<Capability>,
    endpoint:     Option<String>,
    nats_url:     Option<String>,
    hooks:        HookRegistry,
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self {
            name:         None,
            version:      "0.1.0".to_owned(),
            gateway:      None,
            soul_toml:    String::new(),
            capabilities: vec![],
            endpoint:     None,
            nats_url:     None,
            hooks:        HookRegistry::default(),
        }
    }
}

impl AgentBuilder {
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into()); self
    }
    pub fn version(mut self, v: impl Into<String>) -> Self {
        self.version = v.into(); self
    }
    pub fn gateway(mut self, addr: impl Into<String>) -> Self {
        self.gateway = Some(addr.into()); self
    }
    /// TOML content that becomes the agent's soul.
    pub fn soul(mut self, toml: impl Into<String>) -> Self {
        self.soul_toml = toml.into(); self
    }
    pub fn capability(mut self, name: impl Into<String>, desc: impl Into<String>) -> Self {
        self.capabilities.push(Capability::new(name, desc)); self
    }
    /// Endpoint this agent advertises to the gateway.
    /// Defaults to `http://0.0.0.0:<serve_port>` when `serve()` is called.
    pub fn endpoint(mut self, ep: impl Into<String>) -> Self {
        self.endpoint = Some(ep.into()); self
    }

    /// NATS URL for event hooks. If not set, hooks are disabled.
    /// Defaults to `NATS_URL` env var if set.
    pub fn nats_url(mut self, url: impl Into<String>) -> Self {
        self.nats_url = Some(url.into()); self
    }

    /// Called when the reconciler assigns a task to this agent.
    pub fn on_task_assigned<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(TaskAssignedEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        self.hooks.on_task_assigned = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    /// Called when a project snapshot changes (new files synced).
    pub fn on_snapshot_changed<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(SnapshotChangedEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        self.hooks.on_snapshot = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    /// Called on lifecycle events (registered, heartbeat lost, shutdown).
    pub fn on_lifecycle<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(LifecycleEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        self.hooks.on_lifecycle = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    /// Called on gateway-wide broadcast events.
    pub fn on_broadcast<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(BroadcastEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        self.hooks.on_broadcast = Some(Arc::new(move |e| Box::pin(f(e))));
        self
    }

    pub async fn build(self) -> Result<Agent> {
        let name    = self.name.ok_or_else(|| anyhow::anyhow!("agent name is required"))?;
        let gateway = self.gateway.ok_or_else(|| anyhow::anyhow!("gateway address is required"))?;

        // NATS URL: explicit > env var > default
        let nats_url = self.nats_url
            .or_else(|| std::env::var("NATS_URL").ok())
            .unwrap_or_else(|| "nats://127.0.0.1:4222".into());

        let client = GatewayClient::connect(&gateway, &name).await?;
        Ok(Agent {
            name,
            version:      self.version,
            gateway_addr: gateway,
            soul_toml:    self.soul_toml,
            capabilities: self.capabilities,
            endpoint:     self.endpoint,
            client:       Arc::new(RwLock::new(client)),
            nats_url,
            hooks:        self.hooks,
        })
    }
}

// ── Agent ─────────────────────────────────────────────────────────────────────

pub struct Agent {
    pub name:         String,
    pub version:      String,
    pub gateway_addr: String,
    pub soul_toml:    String,
    pub capabilities: Vec<Capability>,
    pub endpoint:     Option<String>,
    client:           Arc<RwLock<GatewayClient>>,
    nats_url:         String,
    hooks:            HookRegistry,
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::default()
    }

    // ── Outbound calls ────────────────────────────────────────────────────────

    pub async fn call(
        &self,
        target: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.call_with(target, payload, CallOptions::default()).await
    }

    pub async fn call_with(
        &self,
        target: &str,
        payload: serde_json::Value,
        opts: CallOptions,
    ) -> Result<serde_json::Value> {
        self.client.write().await.call(target, "invoke", payload, &opts).await
    }

    pub async fn stream(
        &self,
        target: &str,
        payload: serde_json::Value,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>> {
        self.client.write().await.stream(target, "invoke", payload).await
    }

    // ── Identity ──────────────────────────────────────────────────────────────

    pub async fn get_soul(&self) -> Result<Soul> {
        self.client.write().await.get_soul().await
    }

    pub async fn update_memory(&self, content: &str) -> Result<()> {
        self.client.write().await.update_memory(content).await
    }

    pub async fn append_todo(&self, task: &str) -> Result<()> {
        self.client.write().await.append_todo(task).await
    }

    pub async fn list_todos(&self, include_done: bool) -> Result<Vec<TodoItem>> {
        self.client.write().await.list_todos(include_done).await
    }

    // ── Serve ─────────────────────────────────────────────────────────────────

    /// Register with the gateway then start serving inbound calls on `addr`.
    ///
    /// `handler` receives a `Message` and must return a `Result<Response>`.
    /// The agent runs until the process is killed.
    pub async fn serve<F, Fut>(self, handler: F) -> Result<()>
    where
        F: Fn(Message) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Response>> + Send,
    {
        let addr: SocketAddr = self.endpoint
            .as_deref()
            .unwrap_or("0.0.0.0:0")  // random port — caller should set endpoint explicitly
            .trim_start_matches("http://")
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());

        // Register with gateway first
        let manifest = convert::to_manifest(
            &self.name,
            &self.version,
            self.endpoint.as_deref().unwrap_or(""),
            &self.capabilities,
            &self.soul_toml,
        );
        self.client.write().await.register(manifest).await?;
        info!(name = %self.name, %addr, "agent registered and serving");

        // Start NATS hook subscriber if any hooks are registered
        if self.hooks.has_any() {
            let nats_url = self.nats_url.clone();
            let agent_name = self.name.clone();
            let hook_registry = self.hooks.clone();
            tokio::spawn(async move {
                hooks::spawn_hook_subscriber(&nats_url, &agent_name, hook_registry).await;
            });
        }

        let handler = Arc::new(handler);
        let svc = AgentService { handler };
        Server::builder()
            .add_service(LocalGatewayServer::new(svc))
            .serve(addr)
            .await?;
        Ok(())
    }
}

// ── AgentService (tonic impl) ─────────────────────────────────────────────────

/// Minimal tonic server so the agent can receive inbound calls from the gateway.
/// The gateway calls back via the agent's registered endpoint using the same
/// LocalGateway proto — specifically the `Call` RPC.
struct AgentService<F> {
    handler: Arc<F>,
}

#[tonic::async_trait]
impl<F, Fut> LocalGateway for AgentService<F>
where
    F: Fn(Message) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Response>> + Send,
{
    type StreamStream = Pin<Box<dyn Stream<Item = std::result::Result<ProtoStreamChunk, Status>> + Send>>;

    async fn register(
        &self, _req: Request<AgentManifest>,
    ) -> std::result::Result<TonicResponse<RegisterAck>, Status> {
        Err(Status::unimplemented("agents do not accept registrations"))
    }

    async fn heartbeat(
        &self, _req: Request<AgentRef>,
    ) -> std::result::Result<TonicResponse<Ack>, Status> {
        Ok(TonicResponse::new(Ack { ok: true }))
    }

    async fn call(
        &self, req: Request<CallRequest>,
    ) -> std::result::Result<TonicResponse<CallResponse>, Status> {
        let inner = req.into_inner();
        let msg = convert::from_call_request(&inner)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let trace_id = msg.trace_id.clone();
        let resp = (self.handler)(msg).await
            .map_err(|e| Status::internal(e.to_string()))?;
        let result = serde_json::to_vec(&resp.payload)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(TonicResponse::new(CallResponse { result, error: String::new(), trace_id }))
    }

    async fn stream(
        &self, _req: Request<CallRequest>,
    ) -> std::result::Result<TonicResponse<Self::StreamStream>, Status> {
        Err(Status::unimplemented("streaming not yet implemented in SDK AgentService"))
    }

    async fn list_agents(
        &self, _req: Request<Empty>,
    ) -> std::result::Result<TonicResponse<AgentList>, Status> {
        Err(Status::unimplemented("list_agents not served by agents"))
    }

    async fn health(
        &self, _req: Request<Empty>,
    ) -> std::result::Result<TonicResponse<HealthStatus>, Status> {
        Ok(TonicResponse::new(HealthStatus { ok: true, ..Default::default() }))
    }

    async fn get_soul(
        &self, _req: Request<AgentRef>,
    ) -> std::result::Result<TonicResponse<SoulContent>, Status> {
        Err(Status::unimplemented("identity RPCs are gateway-managed"))
    }

    async fn update_memory(
        &self, _req: Request<MemoryPatch>,
    ) -> std::result::Result<TonicResponse<Ack>, Status> {
        Err(Status::unimplemented("identity RPCs are gateway-managed"))
    }

    async fn append_todo(
        &self, _req: Request<TodoEntry>,
    ) -> std::result::Result<TonicResponse<Ack>, Status> {
        Err(Status::unimplemented("identity RPCs are gateway-managed"))
    }

    async fn list_todos(
        &self, _req: Request<TodoFilter>,
    ) -> std::result::Result<TonicResponse<TodoList>, Status> {
        Err(Status::unimplemented("identity RPCs are gateway-managed"))
    }

    async fn complete_todo(
        &self, _req: Request<TodoComplete>,
    ) -> std::result::Result<TonicResponse<Ack>, Status> {
        Err(Status::unimplemented("identity RPCs are gateway-managed"))
    }
}

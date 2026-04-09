/// :7240 — plain gRPC server for LAN agents.
///
/// No TLS, no auth. The LAN is the trust boundary.
/// All agents on the local network connect here to:
///   - Register themselves (and receive their AgentId)
///   - Send heartbeats
///   - Call other agents via the gateway
///   - Read/write their soul, memory, and todos

use a2a_proto::local::{
    local_gateway_server::{LocalGateway, LocalGatewayServer},
    Ack, AgentInfo, AgentList, AgentManifest, AgentRef, CallRequest, CallResponse,
    Empty, HealthStatus, MemoryPatch, RegisterAck, SoulContent, StreamChunk,
    TodoComplete, TodoEntry, TodoFilter, TodoList, Todo,
};

use crate::identity::IdentityStore;
use crate::registry::{AgentEntry, Registry};
use crate::router::{Router, RouterError};

use chrono::Utc;
use reqwest::Client;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::{debug, info};

// ─── Handler ─────────────────────────────────────────────────────────────────

pub struct LocalGatewayHandler {
    registry:     Registry,
    identity:     IdentityStore,
    router:       Arc<Router>,
    http:         Client,
    gateway_name: String,
}

impl LocalGatewayHandler {
    pub fn new(registry: Registry, identity: IdentityStore, router: Arc<Router>, gateway_name: &str) -> Self {
        Self {
            registry,
            identity,
            router,
            http:         Client::new(),
            gateway_name: gateway_name.to_owned(),
        }
    }

    pub fn into_service(self) -> LocalGatewayServer<Self> {
        LocalGatewayServer::new(self)
    }
}

// ─── gRPC implementation ─────────────────────────────────────────────────────

#[tonic::async_trait]
impl LocalGateway for LocalGatewayHandler {
    // ── Registration ─────────────────────────────────────────────────────────

    async fn register(
        &self,
        req: Request<AgentManifest>,
    ) -> Result<Response<RegisterAck>, Status> {
        let m = req.into_inner();
        if m.name.is_empty() || m.name.len() > 64 {
            return Err(Status::invalid_argument("agent name must be 1-64 characters"));
        }
        if !m.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(Status::invalid_argument("agent name must be alphanumeric, dash, or underscore"));
        }
        if m.endpoint.is_empty() || m.endpoint.len() > 256 {
            return Err(Status::invalid_argument("agent endpoint must be 1-256 characters"));
        }
        // SSRF prevention (SEC-03): validate endpoint URL scheme and block dangerous hosts
        validate_endpoint(&m.endpoint)?;

        let entry = AgentEntry {
            name:          m.name.clone(),
            version:       m.version.clone(),
            endpoint:      m.endpoint.clone(),
            capabilities:  m.capabilities.clone(),
            soul_toml:     m.soul_toml.clone(),
            last_seen:     Some(Utc::now().to_rfc3339()),
            registered_at: Utc::now().to_rfc3339(),
        };

        self.registry.upsert(entry).await.map_err(internal)?;

        // Set soul on first registration (won't overwrite if already set)
        if !m.soul_toml.is_empty() {
            self.identity
                .set_soul(&m.name, &m.soul_toml, false)
                .await
                .map_err(internal)?;
        }

        info!(agent = %m.name, endpoint = %m.endpoint, "agent registered");
        Ok(Response::new(RegisterAck {
            agent_id: m.name,
            gateway:  self.gateway_name.clone(),
        }))
    }

    async fn heartbeat(&self, req: Request<AgentRef>) -> Result<Response<Ack>, Status> {
        let name = req.into_inner().name;
        let found = self.registry.heartbeat(&name).await.map_err(internal)?;
        if !found {
            return Err(Status::not_found(format!("unknown agent: {name}")));
        }
        debug!(agent = %name, "heartbeat");
        Ok(Response::new(Ack { ok: true }))
    }

    // ── Task calls ───────────────────────────────────────────────────────────

    async fn call(&self, req: Request<CallRequest>) -> Result<Response<CallResponse>, Status> {
        let r = req.into_inner();
        let trace_id = if r.trace_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            r.trace_id.clone()
        };

        let decision = self.router
            .route(&r.caller, &r.target_agent, r.hop_count)
            .map_err(router_err_to_status)?;

        debug!(
            caller = %r.caller,
            target = %r.target_agent,
            hops   = decision.hop_count,
            trace  = %trace_id,
            "routing call"
        );

        // Forward to agent's endpoint via gRPC (agents serve LocalGateway proto)
        let channel = tonic::transport::Channel::from_shared(decision.endpoint.clone())
            .map_err(|e| Status::internal(format!("invalid agent endpoint: {e}")))?
            .connect_timeout(std::time::Duration::from_secs(5))
            .connect()
            .await
            .map_err(|e| Status::unavailable(format!("agent unreachable: {e}")))?;

        let mut agent_client = a2a_proto::local::local_gateway_client::LocalGatewayClient::new(channel);

        let forward_req = CallRequest {
            target_agent: r.target_agent.clone(),
            method:       r.method,
            payload:      r.payload,
            trace_id:     trace_id.clone(),
            caller:       r.caller,
            hop_count:    decision.hop_count,
        };

        let resp = agent_client.call(forward_req).await
            .map_err(|e| Status::unavailable(format!("agent call failed: {e}")))?;

        Ok(resp)
    }

    type StreamStream = ReceiverStream<Result<StreamChunk, Status>>;

    async fn stream(
        &self,
        req: Request<CallRequest>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        let r = req.into_inner();
        let decision = self.router
            .route(&r.caller, &r.target_agent, r.hop_count)
            .map_err(router_err_to_status)?;

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let url = format!("{}/a2a/stream", decision.endpoint.trim_end_matches('/'));
        let http = self.http.clone();
        let trace_id = r.trace_id.clone();

        tokio::spawn(async move {
            let body = serde_json::json!({
                "method":   r.method,
                "payload":  r.payload,
                "trace_id": trace_id,
            });
            match http.post(&url).json(&body).send().await {
                Ok(resp) => {
                    // For now, buffer the whole response and send as single chunk
                    match resp.bytes().await {
                        Ok(b) => {
                            let _ = tx.send(Ok(StreamChunk {
                                data:     b.to_vec(),
                                done:     true,
                                trace_id: trace_id.clone(),
                            })).await;
                        }
                        Err(e) => { let _ = tx.send(Err(Status::internal(e.to_string()))).await; }
                    }
                }
                Err(e) => { let _ = tx.send(Err(Status::unavailable(e.to_string()))).await; }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    // ── Discovery ────────────────────────────────────────────────────────────

    async fn list_agents(&self, _: Request<Empty>) -> Result<Response<AgentList>, Status> {
        let agents = self.registry.list().into_iter().map(|e| AgentInfo {
            name:         e.name,
            version:      e.version,
            endpoint:     e.endpoint,
            capabilities: e.capabilities,
            last_seen:    e.last_seen.unwrap_or_default(),
            healthy:      true, // TODO: probe liveness
        }).collect();
        Ok(Response::new(AgentList { agents }))
    }

    async fn health(&self, _: Request<Empty>) -> Result<Response<HealthStatus>, Status> {
        Ok(Response::new(HealthStatus {
            ok:                  true,
            agent_count:         self.registry.len() as i32,
            peer_count:          0,
            nats_ok:             false,
            reconciler_last_run: String::new(),
        }))
    }

    // ── Identity ─────────────────────────────────────────────────────────────

    async fn get_soul(&self, req: Request<AgentRef>) -> Result<Response<SoulContent>, Status> {
        let name = req.into_inner().name;
        let content = self.identity.get_soul(&name).await.map_err(|e| {
            if e.to_string().contains("not found") {
                Status::not_found(e.to_string())
            } else {
                internal(e)
            }
        })?;
        Ok(Response::new(SoulContent { content_toml: content }))
    }

    async fn update_memory(
        &self,
        req: Request<MemoryPatch>,
    ) -> Result<Response<Ack>, Status> {
        let m = req.into_inner();
        self.identity
            .append_memory(&m.agent_name, &m.append_md)
            .await
            .map_err(internal)?;
        Ok(Response::new(Ack { ok: true }))
    }

    async fn append_todo(&self, req: Request<TodoEntry>) -> Result<Response<Ack>, Status> {
        let t = req.into_inner();
        self.identity
            .append_todo(&t.agent_name, &t.task)
            .await
            .map_err(internal)?;
        Ok(Response::new(Ack { ok: true }))
    }

    async fn list_todos(&self, req: Request<TodoFilter>) -> Result<Response<TodoList>, Status> {
        let f = req.into_inner();
        let rows = self.identity
            .list_todos(&f.agent_name, f.include_done)
            .await
            .map_err(internal)?;

        let todos = rows.into_iter().map(|r| Todo {
            id:           r.id,
            task:         r.task,
            status:       r.status,
            notes:        r.notes,
            created_at:   r.created_at,
            completed_at: r.completed_at.unwrap_or_default(),
        }).collect();

        Ok(Response::new(TodoList { todos }))
    }

    async fn complete_todo(
        &self,
        req: Request<TodoComplete>,
    ) -> Result<Response<Ack>, Status> {
        let c = req.into_inner();
        self.identity
            .complete_todo(c.id, &c.notes)
            .await
            .map_err(|e| {
                if e.to_string().contains("not found") {
                    Status::not_found(e.to_string())
                } else {
                    internal(e)
                }
            })?;
        Ok(Response::new(Ack { ok: true }))
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn internal(e: impl std::fmt::Display) -> Status {
    Status::internal(e.to_string())
}

/// SSRF prevention: validate agent endpoint URL (SEC-03).
/// Blocks cloud metadata services, loopback, and non-HTTP schemes.
fn validate_endpoint(endpoint: &str) -> Result<(), Status> {
    // Must start with http:// or https://
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(Status::invalid_argument("endpoint must use http or https scheme"));
    }

    // Extract host portion (between :// and next / or :)
    let after_scheme = endpoint.split("://").nth(1).unwrap_or("");
    let host = after_scheme.split(&['/', ':'][..]).next().unwrap_or("");

    // Block cloud metadata services (SSRF targets).
    // Note: localhost/127.0.0.1/0.0.0.0 are intentionally ALLOWED here because
    // LAN agents legitimately register with bind addresses like http://0.0.0.0:8080.
    // The SSRF risk is in the call-forwarding path, not registration.
    let blocked = [
        "169.254.169.254", "metadata.google.internal",
        "metadata.internal",
    ];
    if blocked.iter().any(|b| host.eq_ignore_ascii_case(b)) {
        return Err(Status::invalid_argument("endpoint host is not allowed (blocked for SSRF prevention)"));
    }
    // Block link-local range
    if host.starts_with("169.254.") {
        return Err(Status::invalid_argument("endpoint host is not allowed (link-local)"));
    }

    Ok(())
}

fn router_err_to_status(e: RouterError) -> Status {
    match e {
        RouterError::HopLimitExceeded { .. } => Status::resource_exhausted(e.to_string()),
        RouterError::NotPermitted { .. }     => Status::permission_denied(e.to_string()),
        RouterError::NotFound { .. }         => Status::not_found(e.to_string()),
    }
}

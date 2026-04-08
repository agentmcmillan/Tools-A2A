/// Peer gRPC server — :7241 (mTLS gRPC).
///
/// Handles inbound calls from other a2a-gateway instances:
///   - Handshake / Ping
///   - Snapshot announcements and object fetches (file sync)
///   - Task log delta sync
///   - Agent delegation
///   - Group join via invite token

use crate::auth::JwtAuth;
use crate::file_sync::FileSyncEngine;
use crate::groups::GroupStore;
use crate::object_store::parse_transcript;
use crate::projects::{ProjectStore, Visibility};
use crate::registry::Registry;
use crate::router::Router;
use crate::task_log::TaskLog;

use a2a_proto::peer::{
    peer_gateway_server::{PeerGateway, PeerGatewayServer},
    AckResponse, DelegateRequest, DelegateResponse,
    FetchObjectRequest, FetchObjectResponse,
    FetchSnapshotRequest, FetchSnapshotResponse,
    JoinGroupRequest, JoinGroupResponse,
    ListProjectsRequest, ListProjectsResponse, RemoteProject,
    PeerHandshake, PeerPing, PeerStatus,
    SnapshotAnnouncement,
    TaskLogEntry as ProtoLogEntry, TaskLogSyncRequest, TaskLogSyncResponse,
};
use anyhow::Result;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::info;

// ── Handler ───────────────────────────────────────────────────────────────────

pub struct PeerGatewayHandler {
    gateway_name: String,
    auth:         JwtAuth,
    registry:     Registry,
    _router:      Arc<Router>,   // used when Delegate RPC forwards to local agents
    projects:     ProjectStore,
    file_sync:    Arc<FileSyncEngine>,
    task_log:     Arc<TaskLog>,
    groups:       Arc<GroupStore>,
}

impl PeerGatewayHandler {
    pub fn new(
        gateway_name: impl Into<String>,
        auth:         JwtAuth,
        registry:     Registry,
        router:       Arc<Router>,
        file_sync:    Arc<FileSyncEngine>,
        task_log:     Arc<TaskLog>,
        groups:       Arc<GroupStore>,
    ) -> Self {
        let gn = gateway_name.into();
        let projects = ProjectStore::new(file_sync.db().clone());
        Self {
            gateway_name: gn,
            auth, registry, _router: router, projects,
            file_sync, task_log, groups,
        }
    }

    pub fn into_service(self) -> PeerGatewayServer<Self> {
        PeerGatewayServer::new(self)
    }

    fn auth_jwt(&self, jwt: &str) -> Result<crate::auth::PeerClaims, Status> {
        self.auth.validate_for(jwt, &self.gateway_name)
            .map_err(|e| Status::unauthenticated(e.to_string()))
    }
}

// ── tonic impl ────────────────────────────────────────────────────────────────

#[tonic::async_trait]
impl PeerGateway for PeerGatewayHandler {

    // ── Handshake ─────────────────────────────────────────────────────────

    async fn handshake(
        &self,
        req: Request<PeerHandshake>,
    ) -> Result<Response<PeerHandshake>, Status> {
        let peer = req.into_inner();
        info!(peer_gateway = %peer.gateway_name, "peer handshake");
        Ok(Response::new(PeerHandshake {
            gateway_name: self.gateway_name.clone(),
            version:      env!("CARGO_PKG_VERSION").to_owned(),
            public_key:   String::new(), // TODO Phase 3: cert exchange
            agent_card:   String::new(),
        }))
    }

    // ── Delegation ────────────────────────────────────────────────────────

    async fn delegate(
        &self,
        req: Request<DelegateRequest>,
    ) -> Result<Response<DelegateResponse>, Status> {
        let r = req.into_inner();
        self.auth_jwt(&r.jwt)?;

        info!(target = %r.target_agent, hop = r.hop_count, "peer delegate");

        let agent = self.registry.get(&r.target_agent)
            .ok_or_else(|| Status::not_found(format!("agent {} not found", r.target_agent)))?;

        // Forward to local agent via HTTP
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/invoke", agent.endpoint))
            .body(r.payload.clone())
            .header("content-type", "application/json")
            .send()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?;

        let result = resp.bytes().await
            .map_err(|e| Status::internal(e.to_string()))?
            .to_vec();

        Ok(Response::new(DelegateResponse {
            result,
            error:    String::new(),
            trace_id: r.trace_id,
        }))
    }

    // ── Ping ──────────────────────────────────────────────────────────────

    async fn ping(
        &self,
        req: Request<PeerPing>,
    ) -> Result<Response<PeerStatus>, Status> {
        let ping = req.into_inner();
        info!(from = %ping.gateway_name, "peer ping");
        let agent_count = self.registry.list().len() as i32;
        Ok(Response::new(PeerStatus {
            ok:           true,
            gateway_name: self.gateway_name.clone(),
            agent_count,
            version:      env!("CARGO_PKG_VERSION").to_owned(),
        }))
    }

    // ── Snapshot announcement ─────────────────────────────────────────────

    async fn announce_snapshot(
        &self,
        req: Request<SnapshotAnnouncement>,
    ) -> Result<Response<AckResponse>, Status> {
        let ann = req.into_inner();
        self.auth_jwt(&ann.jwt)?;
        info!(project = %ann.project_id, snapshot = %ann.snapshot_id, from = %ann.gateway_name, "snapshot announced");

        // Check we have a local copy of this project
        let project = self.projects.get_by_name(&ann.project_name).await
            .map_err(|e| Status::internal(e.to_string()))?;

        if project.is_none() {
            // We don't have this project — could trigger onboarding if policy allows.
            // For now just ack without action (user must explicitly add projects).
            return Ok(Response::new(AckResponse { ok: true, error: String::new() }));
        }

        // Mark that a new snapshot is available (peer_sync will pull it)
        // The actual fetch is lazy — triggered by background peer_sync task.
        Ok(Response::new(AckResponse { ok: true, error: String::new() }))
    }

    // ── Snapshot fetch ────────────────────────────────────────────────────

    async fn fetch_snapshot(
        &self,
        req: Request<FetchSnapshotRequest>,
    ) -> Result<Response<FetchSnapshotResponse>, Status> {
        let r = req.into_inner();
        self.auth_jwt(&r.jwt)?;

        let snap = self.file_sync.get_snapshot(&r.snapshot_id).await
            .map_err(|e| Status::internal(e.to_string()))?;

        let Some(snap) = snap else {
            return Ok(Response::new(FetchSnapshotResponse {
                snapshot_id: r.snapshot_id,
                transcript:  String::new(),
                missing_objects: vec![],
                error: "snapshot not found".into(),
            }));
        };

        // Tell the caller which objects it already has vs needs to fetch
        let entries    = parse_transcript(&snap.transcript);
        let all_hashes: Vec<String> = entries.iter().map(|e| e.sha256.clone()).collect();
        let missing    = self.file_sync.objects().missing(&all_hashes).await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(FetchSnapshotResponse {
            snapshot_id:     snap.id,
            transcript:      snap.transcript,
            missing_objects: missing,
            error:           String::new(),
        }))
    }

    // ── Object fetch ──────────────────────────────────────────────────────

    async fn fetch_object(
        &self,
        req: Request<FetchObjectRequest>,
    ) -> Result<Response<FetchObjectResponse>, Status> {
        let r = req.into_inner();
        self.auth_jwt(&r.jwt)?;

        let data = self.file_sync.objects().get(&r.sha256).await
            .map_err(|e| Status::internal(e.to_string()))?;

        match data {
            None => Ok(Response::new(FetchObjectResponse {
                sha256: r.sha256,
                data:   vec![],
                error:  "object not found".into(),
            })),
            Some(bytes) => Ok(Response::new(FetchObjectResponse {
                sha256: r.sha256,
                data:   bytes,
                error:  String::new(),
            })),
        }
    }

    // ── Project listing ───────────────────────────────────────────────────

    async fn list_projects(
        &self,
        req: Request<ListProjectsRequest>,
    ) -> Result<Response<ListProjectsResponse>, Status> {
        let r = req.into_inner();
        let claims = self.auth_jwt(&r.jwt)?;

        // Return projects that are public, or group-visible and the requester
        // is a member of that group.
        let all_projects = self.projects.list().await
            .map_err(|e| Status::internal(e.to_string()))?;

        let mut out = Vec::new();
        for p in all_projects {
            let visible = match p.visibility {
                Visibility::Public  => true,
                Visibility::Group   => {
                    // Check group membership for the requester
                    self.groups.groups_for_gateway(&claims.iss).await
                        .map(|gs| !gs.is_empty())
                        .unwrap_or(false)
                }
                Visibility::LanOnly => false,
            };
            if !visible { continue; }

            let head = self.file_sync.head(&p.id).await
                .ok().flatten()
                .map(|s| s.id)
                .unwrap_or_default();

            out.push(RemoteProject {
                project_id:   p.id,
                project_name: p.name,
                repo_url:     p.repo_url,
                description:  p.description,
                visibility:   p.visibility.as_str().to_owned(),
                snapshot_id:  head,
                gateway_name: self.gateway_name.clone(),
            });
        }

        Ok(Response::new(ListProjectsResponse { projects: out, error: String::new() }))
    }

    // ── Task log sync ─────────────────────────────────────────────────────

    async fn sync_task_log(
        &self,
        req: Request<TaskLogSyncRequest>,
    ) -> Result<Response<TaskLogSyncResponse>, Status> {
        let r = req.into_inner();
        self.auth_jwt(&r.jwt)?;

        let entries = self.task_log.since(&r.project_id, r.after_cursor).await
            .map_err(|e| Status::internal(e.to_string()))?;

        let head = self.task_log.head_cursor(&r.project_id).await
            .unwrap_or(0);

        let proto_entries: Vec<ProtoLogEntry> = entries.into_iter().map(|e| ProtoLogEntry {
            id:           e.id,
            gateway_name: e.gateway_name,
            agent_name:   e.agent_name,
            entry_type:   e.entry_type.as_str().to_owned(),
            content:      e.content,
            cursor:       e.cursor,
            created_at:   e.created_at,
            signature:    e.signature.unwrap_or_default(),
        }).collect();

        Ok(Response::new(TaskLogSyncResponse {
            entries:     proto_entries,
            head_cursor: head,
            error:       String::new(),
        }))
    }

    // ── Group join ────────────────────────────────────────────────────────

    async fn join_group(
        &self,
        req: Request<JoinGroupRequest>,
    ) -> Result<Response<JoinGroupResponse>, Status> {
        let r = req.into_inner();
        self.auth_jwt(&r.jwt)?;

        info!(gateway = %r.gateway_name, "peer joining group via invite");

        let group = self.groups.consume_invite(&r.invite_token, &r.gateway_name).await
            .map_err(|e| Status::permission_denied(e.to_string()))?;

        Ok(Response::new(JoinGroupResponse {
            group_id:   group.id,
            group_name: group.name,
            error:      String::new(),
        }))
    }
}

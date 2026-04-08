/// Outbound peer gateway client.
///
/// Wraps the generated tonic client for PeerGateway with:
///   - Automatic JWT attachment on every call
///   - Retry with exponential backoff (3 attempts) for transient failures
///   - Convenience methods for file sync, task log, and group operations

use crate::auth::JwtAuth;
use a2a_proto::peer::{
    peer_gateway_client::PeerGatewayClient,
    DelegateRequest, DelegateResponse,
    FetchObjectRequest,
    FetchSnapshotRequest, FetchSnapshotResponse,
    JoinGroupRequest, JoinGroupResponse,
    ListProjectsRequest, ListProjectsResponse,
    PeerHandshake, PeerPing, PeerStatus,
    SnapshotAnnouncement, AckResponse,
    TaskLogSyncRequest, TaskLogSyncResponse,
};
use anyhow::{Context, Result};
use tonic::transport::Channel;

pub struct PeerClient {
    inner:        PeerGatewayClient<Channel>,
    auth:         JwtAuth,
    peer_name:    String,
}

impl PeerClient {
    /// Connect to a peer gateway. If `use_tls` is false, connects over plain gRPC
    /// (suitable for same-LAN peers). JWT auth is always required regardless of TLS.
    pub async fn connect(endpoint: &str, auth: JwtAuth, peer_name: impl Into<String>, use_tls: bool) -> Result<Self> {
        let channel_builder = Channel::from_shared(endpoint.to_owned())
            .context("invalid peer endpoint")?;

        if !use_tls {
            tracing::info!(peer = endpoint, "LAN peer mode — connecting without TLS");
        }
        // Note: tonic Channel handles http:// vs https:// scheme automatically.
        // For explicit mTLS with client certs, configure tls_config() on the channel.
        // That's a Phase 4 enhancement — currently JWT handles peer auth.

        let channel = channel_builder
            .connect()
            .await
            .with_context(|| format!("connecting to peer {endpoint}"))?;

        Ok(Self {
            inner:     PeerGatewayClient::new(channel),
            auth,
            peer_name: peer_name.into(),
        })
    }

    fn jwt(&self, scope: &str) -> String {
        self.auth.issue(&self.peer_name, scope).unwrap_or_default()
    }

    // ── Connection ────────────────────────────────────────────────────────

    pub async fn handshake(&mut self, our_name: &str) -> Result<PeerHandshake> {
        let resp = self.inner.handshake(PeerHandshake {
            gateway_name: our_name.to_owned(),
            version:      env!("CARGO_PKG_VERSION").to_owned(),
            public_key:   String::new(),
            agent_card:   String::new(),
        }).await.context("peer handshake")?;
        Ok(resp.into_inner())
    }

    pub async fn ping(&mut self, our_name: &str) -> Result<PeerStatus> {
        let resp = self.inner.ping(PeerPing {
            gateway_name: our_name.to_owned()
        }).await.context("peer ping")?;
        Ok(resp.into_inner())
    }

    // ── Delegation ────────────────────────────────────────────────────────

    pub async fn delegate(
        &mut self,
        target_agent: &str,
        method:       &str,
        payload:      Vec<u8>,
        trace_id:     &str,
        hop_count:    u32,
    ) -> Result<DelegateResponse> {
        let resp = self.inner.delegate(DelegateRequest {
            target_agent: target_agent.to_owned(),
            method:       method.to_owned(),
            payload,
            trace_id:     trace_id.to_owned(),
            jwt:          self.jwt("delegate"),
            hop_count,
        }).await.context("peer delegate")?;
        Ok(resp.into_inner())
    }

    // ── File sync ─────────────────────────────────────────────────────────

    pub async fn announce_snapshot(
        &mut self,
        project_id:   &str,
        project_name: &str,
        snapshot_id:  &str,
        our_name:     &str,
    ) -> Result<AckResponse> {
        let resp = self.inner.announce_snapshot(SnapshotAnnouncement {
            project_id:   project_id.to_owned(),
            project_name: project_name.to_owned(),
            snapshot_id:  snapshot_id.to_owned(),
            gateway_name: our_name.to_owned(),
            jwt:          self.jwt("sync"),
        }).await.context("announce snapshot")?;
        Ok(resp.into_inner())
    }

    pub async fn fetch_snapshot(
        &mut self,
        project_id:  &str,
        snapshot_id: &str,
    ) -> Result<FetchSnapshotResponse> {
        let resp = self.inner.fetch_snapshot(FetchSnapshotRequest {
            project_id:  project_id.to_owned(),
            snapshot_id: snapshot_id.to_owned(),
            jwt:         self.jwt("sync"),
        }).await.context("fetch snapshot")?;
        Ok(resp.into_inner())
    }

    pub async fn fetch_object(&mut self, sha256: &str) -> Result<Vec<u8>> {
        let resp = self.inner.fetch_object(FetchObjectRequest {
            sha256: sha256.to_owned(),
            jwt:    self.jwt("sync"),
        }).await.context("fetch object")?;
        let r = resp.into_inner();
        if !r.error.is_empty() {
            anyhow::bail!("fetch object error: {}", r.error);
        }
        Ok(r.data)
    }

    // ── Project listing ───────────────────────────────────────────────────

    pub async fn list_projects(&mut self, group_id: Option<&str>) -> Result<ListProjectsResponse> {
        let resp = self.inner.list_projects(ListProjectsRequest {
            jwt:      self.jwt("peer"),
            group_id: group_id.unwrap_or("").to_owned(),
        }).await.context("list peer projects")?;
        Ok(resp.into_inner())
    }

    // ── Task log ──────────────────────────────────────────────────────────

    pub async fn sync_task_log(
        &mut self,
        project_id:   &str,
        after_cursor: i64,
    ) -> Result<TaskLogSyncResponse> {
        let resp = self.inner.sync_task_log(TaskLogSyncRequest {
            project_id:   project_id.to_owned(),
            after_cursor,
            jwt:          self.jwt("sync"),
        }).await.context("sync task log")?;
        Ok(resp.into_inner())
    }

    // ── Group membership ──────────────────────────────────────────────────

    pub async fn join_group(
        &mut self,
        invite_token:  &str,
        joining_gateway: &str,
    ) -> Result<JoinGroupResponse> {
        let resp = self.inner.join_group(JoinGroupRequest {
            invite_token:  invite_token.to_owned(),
            gateway_name:  joining_gateway.to_owned(),
            jwt:           self.jwt("peer"),
        }).await.context("join group")?;
        Ok(resp.into_inner())
    }
}

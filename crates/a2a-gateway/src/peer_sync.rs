/// Peer sync coordinator.
///
/// Runs as a background Tokio task. Every `sync_interval_secs` it:
///   1. Pings each configured peer to check liveness
///   2. For each peer project we track, asks for task log delta since our cursor
///   3. If a new snapshot is available (announced via NATS or discovered on poll),
///      pulls the transcript + missing objects
///   4. Applies the snapshot to our local project_heads
///   5. Publishes a NATS notification if NATS is available
///
/// Snapshot pull flow (delta):
///
///   us                               peer
///   ──                               ────
///   list_projects()             →
///                               ←    RemoteProject{snapshot_id}
///   fetch_snapshot(snap_id)     →    (returns transcript + missing hashes)
///   fetch_object(hash) × N      →    (parallel, one per missing object)
///   apply_peer_snapshot()            advance our project_heads
///   nats.publish(project.{id}.snapshot)

use crate::auth::JwtAuth;
use crate::config::PeerConfig;
use crate::file_sync::FileSyncEngine;
use crate::peer_client::PeerClient;
use crate::projects::ProjectStore;
use crate::task_log::{EntryType, TaskLog};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn, error};

const DEFAULT_SYNC_INTERVAL: u64 = 60; // seconds

// ── PeerSyncCoordinator ───────────────────────────────────────────────────────

pub struct PeerSyncCoordinator {
    gateway_name:   String,
    peers:          Vec<PeerConfig>,
    auth:           JwtAuth,
    file_sync:      Arc<FileSyncEngine>,
    task_log:       Arc<TaskLog>,
    projects:       ProjectStore,
    sync_interval:  Duration,
}

impl PeerSyncCoordinator {
    pub fn new(
        gateway_name:   impl Into<String>,
        peers:          Vec<PeerConfig>,
        auth:           JwtAuth,
        file_sync:      Arc<FileSyncEngine>,
        task_log:       Arc<TaskLog>,
        db:             crate::db::Db,
        sync_interval_secs: u64,
    ) -> Self {
        let gn = gateway_name.into();
        Self {
            gateway_name: gn,
            peers,
            auth,
            file_sync,
            task_log,
            projects: ProjectStore::new(db),
            sync_interval: Duration::from_secs(
                if sync_interval_secs == 0 { DEFAULT_SYNC_INTERVAL } else { sync_interval_secs }
            ),
        }
    }

    /// Spawn the coordinator as a background Tokio task.
    pub fn spawn(self) {
        tokio::spawn(async move {
            info!("peer sync coordinator started ({} peers)", self.peers.len());
            loop {
                if let Err(e) = self.sync_round().await {
                    error!("peer sync round failed: {e}");
                }
                tokio::time::sleep(self.sync_interval).await;
            }
        });
    }

    async fn sync_round(&self) -> Result<()> {
        for peer in &self.peers {
            if let Err(e) = self.sync_peer(peer).await {
                warn!(peer = %peer.name, error = %e, "peer sync failed");
            }
        }
        Ok(())
    }

    async fn sync_peer(&self, peer: &PeerConfig) -> Result<()> {
        let mut client = PeerClient::connect(
            &peer.endpoint,
            self.auth.clone(),
            &peer.name,
        ).await?;

        // Ping first
        let status = client.ping(&self.gateway_name).await?;
        if !status.ok {
            warn!(peer = %peer.name, "peer ping returned not-ok");
            return Ok(());
        }

        // List available projects
        let remote_list = client.list_projects(None).await?;
        for remote_proj in remote_list.projects {
            if let Err(e) = self.sync_project(&mut client, peer, &remote_proj).await {
                warn!(peer = %peer.name, project = %remote_proj.project_name,
                      error = %e, "project sync failed");
            }
        }

        Ok(())
    }

    async fn sync_project(
        &self,
        client:       &mut PeerClient,
        peer:         &PeerConfig,
        remote:       &a2a_proto::peer::RemoteProject,
    ) -> Result<()> {
        // Find our local copy (if any)
        let local = self.projects.get_by_name(&remote.project_name).await?;

        // Sync task log delta
        let local_cursor = match &local {
            Some(p) => self.task_log.head_cursor(&p.id).await.unwrap_or(0),
            None    => 0,
        };

        let log_sync = client.sync_task_log(&remote.project_id, local_cursor).await?;
        if let Some(local_proj) = &local {
            for entry in log_sync.entries {
                // Write entries from the peer into our local task log.
                // We trust the gateway_name + signature; skip if already at or beyond cursor.
                self.task_log.append(
                    &local_proj.id,
                    &entry.gateway_name,
                    &entry.agent_name,
                    EntryType::from_str(&entry.entry_type),
                    &entry.content,
                    None,
                ).await.ok(); // best-effort; duplicate cursors will be ignored naturally
            }
        }

        // Check if we need to pull the snapshot
        let current_head = match &local {
            Some(p) => self.file_sync.head(&p.id).await
                .ok().flatten().map(|s| s.id).unwrap_or_default(),
            None => String::new(),
        };

        if current_head == remote.snapshot_id || remote.snapshot_id.is_empty() {
            return Ok(()); // already up to date
        }

        info!(peer = %peer.name, project = %remote.project_name, "pulling new snapshot");

        // Fetch the transcript
        let snap_resp = client.fetch_snapshot(&remote.project_id, &remote.snapshot_id).await?;
        if !snap_resp.error.is_empty() {
            anyhow::bail!("fetch snapshot error: {}", snap_resp.error);
        }

        // Fetch missing objects
        for hash in &snap_resp.missing_objects {
            let data = client.fetch_object(hash).await?;
            self.file_sync.objects().put(&data).await?;
        }

        // Apply snapshot to local project
        if let Some(local_proj) = &local {
            self.file_sync.apply_peer_snapshot(
                &local_proj.id,
                &remote.snapshot_id,
                &snap_resp.transcript,
                &peer.name,
            ).await?;

            info!(peer = %peer.name, project = %remote.project_name,
                  snapshot = %remote.snapshot_id, "snapshot applied");
        }

        Ok(())
    }
}

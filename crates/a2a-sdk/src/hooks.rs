/// Agent hook system — reactive event callbacks via NATS subscriptions.
///
/// ```text
///   Gateway (reconciler / file_sync / registry)
///       │
///       ▼  NATS publish
///   a2a.agent.{name}.task_assigned    ← reconciler assigns a task
///   a2a.agent.{name}.snapshot         ← project snapshot changed
///   a2a.agent.{name}.lifecycle        ← registered / heartbeat_lost
///   a2a.broadcast.announcement        ← gateway-wide announcements
///       │
///       ▼  NATS subscribe (inside Agent::serve)
///   HookRegistry dispatches to user callbacks
/// ```
///
/// Usage:
/// ```rust,no_run
/// let agent = Agent::builder()
///     .name("research")
///     .gateway("http://gateway:7240")
///     .on_task_assigned(|event| async move {
///         println!("assigned: {}", event.task);
///         Ok(())
///     })
///     .on_snapshot_changed(|event| async move {
///         println!("project {} changed", event.project_id);
///         Ok(())
///     })
///     .build()
///     .await?;
/// ```

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// ── Event types ─────────────────────────────────────────────────────────────

/// A task was assigned to this agent by the reconciler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssignedEvent {
    pub project_id:  String,
    pub task:        String,
    pub assigned_by: String,  // gateway name
    pub todo_id:     Option<i64>,
}

/// A project snapshot changed (new files synced).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotChangedEvent {
    pub project_id:   String,
    pub project_name: String,
    pub snapshot_id:  String,
    pub gateway_name: String,
}

/// Agent lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub event_type: LifecycleType,
    pub agent_name: String,
    pub gateway:    String,
    pub message:    String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleType {
    Registered,
    HeartbeatLost,
    Shutdown,
}

/// Gateway-wide broadcast (e.g. maintenance notice).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastEvent {
    pub gateway:    String,
    pub event_type: String,
    pub message:    String,
}

// ── Callback types ──────────────────────────────────────────────────────────

pub type BoxFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
pub type TaskAssignedFn   = Arc<dyn Fn(TaskAssignedEvent)   -> BoxFuture + Send + Sync>;
pub type SnapshotFn       = Arc<dyn Fn(SnapshotChangedEvent) -> BoxFuture + Send + Sync>;
pub type LifecycleFn      = Arc<dyn Fn(LifecycleEvent)       -> BoxFuture + Send + Sync>;
pub type BroadcastFn      = Arc<dyn Fn(BroadcastEvent)       -> BoxFuture + Send + Sync>;

// ── Hook registry ───────────────────────────────────────────────────────────

/// Stores user-registered hook callbacks. Built via AgentBuilder, consumed by Agent::serve().
#[derive(Default, Clone)]
pub struct HookRegistry {
    pub on_task_assigned:   Option<TaskAssignedFn>,
    pub on_snapshot:        Option<SnapshotFn>,
    pub on_lifecycle:       Option<LifecycleFn>,
    pub on_broadcast:       Option<BroadcastFn>,
}

impl HookRegistry {
    pub fn has_any(&self) -> bool {
        self.on_task_assigned.is_some()
            || self.on_snapshot.is_some()
            || self.on_lifecycle.is_some()
            || self.on_broadcast.is_some()
    }

    /// NATS subjects this agent should subscribe to.
    pub fn subjects(&self, agent_name: &str) -> Vec<String> {
        let mut subs = Vec::new();
        if self.on_task_assigned.is_some() {
            subs.push(format!("a2a.agent.{agent_name}.task_assigned"));
        }
        if self.on_snapshot.is_some() {
            subs.push(format!("a2a.agent.{agent_name}.snapshot"));
        }
        if self.on_lifecycle.is_some() {
            subs.push(format!("a2a.agent.{agent_name}.lifecycle"));
        }
        if self.on_broadcast.is_some() {
            subs.push("a2a.broadcast.>".to_owned());
        }
        subs
    }
}

// ── NATS subscriber (spawned inside Agent::serve) ───────────────────────────

use futures::StreamExt;

/// Hook event kind — used for exact-match dispatch instead of fragile substring matching.
#[derive(Debug, Clone)]
enum HookKind {
    TaskAssigned,
    Snapshot,
    Lifecycle,
    Broadcast,
}

/// Connect to NATS and dispatch events to hook callbacks.
/// Runs as a background tokio task. Graceful: if NATS is down, logs and returns.
pub async fn spawn_hook_subscriber(
    nats_url:   &str,
    agent_name: &str,
    hooks:      HookRegistry,
) {
    let client = match async_nats::connect(nats_url).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(nats_url, error = %e, "hooks disabled — NATS not available");
            return;
        }
    };

    tracing::info!(nats_url, agent = agent_name, "hook subscriber connected to NATS");

    // Build (subject, kind) pairs for exact-match dispatch
    let mut subscriptions: Vec<(String, HookKind)> = Vec::new();
    if hooks.on_task_assigned.is_some() {
        subscriptions.push((format!("a2a.agent.{agent_name}.task_assigned"), HookKind::TaskAssigned));
    }
    if hooks.on_snapshot.is_some() {
        subscriptions.push((format!("a2a.agent.{agent_name}.snapshot"), HookKind::Snapshot));
    }
    if hooks.on_lifecycle.is_some() {
        subscriptions.push((format!("a2a.agent.{agent_name}.lifecycle"), HookKind::Lifecycle));
    }
    if hooks.on_broadcast.is_some() {
        subscriptions.push(("a2a.broadcast.>".to_owned(), HookKind::Broadcast));
    }

    for (subject, kind) in subscriptions {
        let mut sub = match client.subscribe(subject.clone()).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(subject, error = %e, "failed to subscribe");
                continue;
            }
        };

        let hooks = hooks.clone();
        let sub_name = subject.clone();
        // Supervised task: catches panics and logs them instead of silently dying
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                while let Some(msg) = sub.next().await {
                    dispatch_event(&kind, &msg.payload, &hooks).await;
                }
            });
            // If the future completes (NATS disconnected), log it
            futures::FutureExt::catch_unwind(result).await.ok();
            tracing::warn!(subject = %sub_name, "hook subscriber task exited");
        });
    }
}

/// Dispatch a NATS message to the right hook callback based on exact kind.
async fn dispatch_event(kind: &HookKind, payload: &[u8], hooks: &HookRegistry) {
    match kind {
        HookKind::TaskAssigned => {
            if let Some(ref cb) = hooks.on_task_assigned {
                match serde_json::from_slice::<TaskAssignedEvent>(payload) {
                    Ok(event) => {
                        if let Err(e) = cb(event).await {
                            tracing::warn!(error = %e, "task_assigned hook error");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "bad task_assigned payload"),
                }
            }
        }
        HookKind::Snapshot => {
            if let Some(ref cb) = hooks.on_snapshot {
                match serde_json::from_slice::<SnapshotChangedEvent>(payload) {
                    Ok(event) => {
                        if let Err(e) = cb(event).await {
                            tracing::warn!(error = %e, "snapshot hook error");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "bad snapshot payload"),
                }
            }
        }
        HookKind::Lifecycle => {
            if let Some(ref cb) = hooks.on_lifecycle {
                match serde_json::from_slice::<LifecycleEvent>(payload) {
                    Ok(event) => {
                        if let Err(e) = cb(event).await {
                            tracing::warn!(error = %e, "lifecycle hook error");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "bad lifecycle payload"),
                }
            }
        }
        HookKind::Broadcast => {
            if let Some(ref cb) = hooks.on_broadcast {
                match serde_json::from_slice::<BroadcastEvent>(payload) {
                    Ok(event) => {
                        if let Err(e) = cb(event).await {
                            tracing::warn!(error = %e, "broadcast hook error");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "bad broadcast payload"),
                }
            }
        }
    }
}

use async_nats::Client;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ── Message types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEvent {
    pub project_id:   String,
    pub project_name: String,
    pub snapshot_id:  String,
    pub gateway_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLogEvent {
    pub project_id:   String,
    pub gateway_name: String,
    pub head_cursor:  i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayOnlineEvent {
    pub gateway_name: String,
    pub agent_count:  usize,
    pub version:      String,
}

// ── NatsBus ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct NatsBus {
    client:       Client,
    gateway_name: String,
}

impl NatsBus {
    /// Connect to NATS. Returns None (not an error) if NATS is unavailable.
    /// If NATS_USER and NATS_PASSWORD env vars are set, authenticates with user/pass.
    pub async fn connect(url: &str, gateway_name: impl Into<String>) -> Option<Self> {
        let connect_result = match (
            std::env::var("NATS_USER").ok(),
            std::env::var("NATS_PASSWORD").ok(),
        ) {
            (Some(user), Some(pass)) if !user.is_empty() && !pass.is_empty() => {
                info!(nats_url = %url, user = %user, "NATS connecting with auth");
                async_nats::ConnectOptions::with_user_and_password(user, pass)
                    .connect(url)
                    .await
            }
            _ => async_nats::connect(url).await,
        };

        match connect_result {
            Ok(client) => {
                let gn = gateway_name.into();
                info!(nats_url = %url, gateway = %gn, "NATS connected");
                Some(Self { client, gateway_name: gn })
            }
            Err(e) => {
                warn!(nats_url = %url, error = %e, "NATS unavailable — notifications disabled");
                None
            }
        }
    }

    // ── Publish ───────────────────────────────────────────────────────────

    pub async fn publish_snapshot(&self, project_id: &str, project_name: &str, snapshot_id: &str) {
        let event = SnapshotEvent {
            project_id:   project_id.to_owned(),
            project_name: project_name.to_owned(),
            snapshot_id:  snapshot_id.to_owned(),
            gateway_name: self.gateway_name.clone(),
        };
        self.publish(
            &format!("a2a.projects.{project_id}.snapshot"),
            &event,
        ).await;
    }

    pub async fn publish_task_log(&self, project_id: &str, head_cursor: i64) {
        let event = TaskLogEvent {
            project_id:   project_id.to_owned(),
            gateway_name: self.gateway_name.clone(),
            head_cursor,
        };
        self.publish(
            &format!("a2a.projects.{project_id}.tasklog"),
            &event,
        ).await;
    }

    /// Notify a specific agent that a task was assigned to them.
    pub async fn publish_task_assigned(
        &self,
        agent_name: &str,
        project_id: &str,
        task: &str,
        todo_id: Option<i64>,
    ) {
        #[derive(Serialize)]
        struct Evt { project_id: String, task: String, assigned_by: String, todo_id: Option<i64> }
        self.publish(
            &format!("a2a.agent.{agent_name}.task_assigned"),
            &Evt {
                project_id:  project_id.to_owned(),
                task:        task.to_owned(),
                assigned_by: self.gateway_name.clone(),
                todo_id,
            },
        ).await;
    }

    /// Notify a specific agent that a project snapshot changed.
    pub async fn publish_agent_snapshot(
        &self,
        agent_name:   &str,
        project_id:   &str,
        project_name: &str,
        snapshot_id:  &str,
    ) {
        self.publish(
            &format!("a2a.agent.{agent_name}.snapshot"),
            &SnapshotEvent {
                project_id:   project_id.to_owned(),
                project_name: project_name.to_owned(),
                snapshot_id:  snapshot_id.to_owned(),
                gateway_name: self.gateway_name.clone(),
            },
        ).await;
    }

    pub async fn publish_online(&self, agent_count: usize) {
        let event = GatewayOnlineEvent {
            gateway_name: self.gateway_name.clone(),
            agent_count,
            version:      env!("CARGO_PKG_VERSION").to_owned(),
        };
        self.publish(
            &format!("a2a.gateway.{}.online", self.gateway_name),
            &event,
        ).await;
    }

    async fn publish<T: Serialize>(&self, subject: &str, payload: &T) {
        match serde_json::to_vec(payload) {
            Ok(bytes) => {
                if let Err(e) = self.client.publish(subject.to_owned(), bytes.into()).await {
                    warn!(subject, error = %e, "NATS publish failed");
                }
            }
            Err(e) => warn!(subject, error = %e, "failed to serialise NATS payload"),
        }
    }
}

// ── NullBus (no-op when NATS is down) ────────────────────────────────────────

/// A no-op bus used when NATS is not available.
/// Allows callers to use the same code path regardless.
pub enum Bus {
    Nats(NatsBus),
    Null,
}

impl Bus {
    pub fn from_option(inner: Option<NatsBus>) -> Self {
        match inner {
            Some(n) => Self::Nats(n),
            None    => Self::Null,
        }
    }

    pub async fn publish_snapshot(&self, project_id: &str, project_name: &str, snapshot_id: &str) {
        if let Self::Nats(n) = self {
            n.publish_snapshot(project_id, project_name, snapshot_id).await;
        }
    }

    pub async fn publish_task_log(&self, project_id: &str, head_cursor: i64) {
        if let Self::Nats(n) = self {
            n.publish_task_log(project_id, head_cursor).await;
        }
    }

    pub async fn publish_online(&self, agent_count: usize) {
        if let Self::Nats(n) = self {
            n.publish_online(agent_count).await;
        }
    }

    pub async fn publish_task_assigned(&self, agent_name: &str, project_id: &str, task: &str, todo_id: Option<i64>) {
        if let Self::Nats(n) = self {
            n.publish_task_assigned(agent_name, project_id, task, todo_id).await;
        }
    }

    pub async fn publish_agent_snapshot(&self, agent_name: &str, project_id: &str, project_name: &str, snapshot_id: &str) {
        if let Self::Nats(n) = self {
            n.publish_agent_snapshot(agent_name, project_id, project_name, snapshot_id).await;
        }
    }
}

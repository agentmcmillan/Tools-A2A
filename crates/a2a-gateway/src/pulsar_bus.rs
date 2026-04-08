/// Apache Pulsar event bus — persistent, geo-replicated message delivery.
///
/// Replaces NATS for production deployments where guaranteed delivery and
/// cross-datacenter replication are required. NATS remains as a lightweight
/// fallback for single-site/dev setups.
///
/// Topic structure:
///
///   persistent://a2a/projects/{project_id}/snapshots    — snapshot announcements
///   persistent://a2a/projects/{project_id}/tasklog      — task log entries
///   persistent://a2a/agents/{agent_name}/tasks           — task assignment hooks
///   persistent://a2a/agents/{agent_name}/events          — lifecycle + snapshot hooks
///   persistent://a2a/gateway/{gateway_name}/online        — gateway liveness
///   persistent://a2a/broadcast/announcements              — gateway-wide broadcasts
///
/// Each gateway subscribes with a unique subscription name (its gateway_name)
/// which gives it its own cursor — messages are retained until acknowledged.
/// This means a gateway that goes offline for hours will receive all missed
/// messages when it reconnects. NATS cannot do this.
///
/// Geo-replication: Pulsar's built-in geo-replication replicates topics across
/// clusters. Configure in pulsar's broker.conf, not in this code.

use crate::util::now_secs;
use anyhow::{Context, Result};
use pulsar::{
    producer, Authentication, Consumer, DeserializeMessage, Payload, Pulsar,
    SerializeMessage, SubType, TokioExecutor,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ── Message types (shared with nats_bus for compat) ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEvent {
    pub project_id:   String,
    pub project_name: String,
    pub snapshot_id:  String,
    pub gateway_name: String,
    pub timestamp:    f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLogEvent {
    pub project_id:   String,
    pub gateway_name: String,
    pub head_cursor:  i64,
    pub timestamp:    f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssignedEvent {
    pub project_id:  String,
    pub task:        String,
    pub assigned_by: String,
    pub agent_name:  String,
    pub todo_id:     Option<i64>,
    pub timestamp:   f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayOnlineEvent {
    pub gateway_name: String,
    pub agent_count:  usize,
    pub version:      String,
    pub timestamp:    f64,
}

// ── SerializeMessage impls for Pulsar ───────────────────────────────────────

impl SerializeMessage for SnapshotEvent {
    fn serialize_message(input: Self) -> Result<producer::Message, pulsar::Error> {
        let bytes = serde_json::to_vec(&input).map_err(|e| pulsar::Error::Custom(e.to_string()))?;
        Ok(producer::Message { payload: bytes, ..Default::default() })
    }
}

impl SerializeMessage for TaskLogEvent {
    fn serialize_message(input: Self) -> Result<producer::Message, pulsar::Error> {
        let bytes = serde_json::to_vec(&input).map_err(|e| pulsar::Error::Custom(e.to_string()))?;
        Ok(producer::Message { payload: bytes, ..Default::default() })
    }
}

impl SerializeMessage for TaskAssignedEvent {
    fn serialize_message(input: Self) -> Result<producer::Message, pulsar::Error> {
        let bytes = serde_json::to_vec(&input).map_err(|e| pulsar::Error::Custom(e.to_string()))?;
        Ok(producer::Message { payload: bytes, ..Default::default() })
    }
}

impl SerializeMessage for GatewayOnlineEvent {
    fn serialize_message(input: Self) -> Result<producer::Message, pulsar::Error> {
        let bytes = serde_json::to_vec(&input).map_err(|e| pulsar::Error::Custom(e.to_string()))?;
        Ok(producer::Message { payload: bytes, ..Default::default() })
    }
}

impl DeserializeMessage for TaskAssignedEvent {
    type Output = Result<Self, serde_json::Error>;
    fn deserialize_message(payload: &Payload) -> Self::Output {
        serde_json::from_slice(&payload.data)
    }
}

// ── PulsarBus ───────────────────────────────────────────────────────────────

pub struct PulsarBus {
    client:        Pulsar<TokioExecutor>,
    gateway_name:  String,
}

impl PulsarBus {
    /// Connect to Pulsar. Returns None (not an error) if Pulsar is unavailable.
    /// `url` should be like `pulsar://127.0.0.1:6650` or `pulsar+ssl://...`
    pub async fn connect(url: &str, gateway_name: impl Into<String>) -> Option<Self> {
        let gn = gateway_name.into();

        let mut builder = Pulsar::builder(url, TokioExecutor);

        // Optional auth via PULSAR_TOKEN env var
        if let Ok(token) = std::env::var("PULSAR_TOKEN") {
            if !token.is_empty() {
                builder = builder.with_auth(Authentication {
                    name: "token".to_owned(),
                    data: token.into_bytes(),
                });
            }
        }

        match builder.build().await {
            Ok(client) => {
                info!(pulsar_url = %url, gateway = %gn, "Pulsar connected");
                Some(Self { client, gateway_name: gn })
            }
            Err(e) => {
                warn!(pulsar_url = %url, error = %e, "Pulsar unavailable — falling back to NATS/polling");
                None
            }
        }
    }

    // ── Publish ────────────────────────────────────────────────────────────

    pub async fn publish_snapshot(&self, project_id: &str, project_name: &str, snapshot_id: &str) {
        let topic = format!("persistent://a2a/projects/{project_id}/snapshots");
        let event = SnapshotEvent {
            project_id:   project_id.to_owned(),
            project_name: project_name.to_owned(),
            snapshot_id:  snapshot_id.to_owned(),
            gateway_name: self.gateway_name.clone(),
            timestamp:    now_secs(),
        };
        self.send(&topic, event).await;
    }

    pub async fn publish_task_log(&self, project_id: &str, head_cursor: i64) {
        let topic = format!("persistent://a2a/projects/{project_id}/tasklog");
        let event = TaskLogEvent {
            project_id:   project_id.to_owned(),
            gateway_name: self.gateway_name.clone(),
            head_cursor,
            timestamp:    now_secs(),
        };
        self.send(&topic, event).await;
    }

    pub async fn publish_task_assigned(
        &self,
        agent_name: &str,
        project_id: &str,
        task: &str,
        todo_id: Option<i64>,
    ) {
        let topic = format!("persistent://a2a/agents/{agent_name}/tasks");
        let event = TaskAssignedEvent {
            project_id:  project_id.to_owned(),
            task:        task.to_owned(),
            assigned_by: self.gateway_name.clone(),
            agent_name:  agent_name.to_owned(),
            todo_id,
            timestamp:   now_secs(),
        };
        self.send(&topic, event).await;
    }

    pub async fn publish_online(&self, agent_count: usize) {
        let topic = format!("persistent://a2a/gateway/{}/online", self.gateway_name);
        let event = GatewayOnlineEvent {
            gateway_name: self.gateway_name.clone(),
            agent_count,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            timestamp: now_secs(),
        };
        self.send(&topic, event).await;
    }

    /// Create a consumer for task assignment events (used by SDK hooks).
    pub async fn subscribe_task_assigned(
        &self,
        agent_name: &str,
    ) -> Result<Consumer<TaskAssignedEvent, TokioExecutor>> {
        let topic = format!("persistent://a2a/agents/{agent_name}/tasks");
        let consumer: Consumer<TaskAssignedEvent, TokioExecutor> = self.client
            .consumer()
            .with_topic(&topic)
            .with_subscription_type(SubType::Exclusive)
            .with_subscription(format!("{}-{agent_name}", self.gateway_name))
            .build()
            .await
            .context("subscribe to task assignments")?;
        Ok(consumer)
    }

    // ── Internal ───────────────────────────────────────────────────────────

    async fn send<T: SerializeMessage + std::fmt::Debug>(&self, topic: &str, event: T) {
        match self.client.producer()
            .with_topic(topic)
            .build()
            .await
        {
            Ok(mut producer) => {
                if let Err(e) = producer.send_non_blocking(event).await {
                    warn!(topic, error = %e, "Pulsar publish failed");
                }
            }
            Err(e) => warn!(topic, error = %e, "Pulsar producer creation failed"),
        }
    }
}

// ── Unified EventBus ────────────────────────────────────────────────────────

/// Unified event bus that tries Pulsar first, falls back to NATS, then no-ops.
///
/// ```text
///   EventBus::Pulsar(PulsarBus)   — persistent, geo-replicated, guaranteed delivery
///   EventBus::Nats(NatsBus)       — lightweight, fire-and-forget, LAN-only
///   EventBus::Null                — silent no-op (dev mode, no message bus configured)
/// ```
pub enum EventBus {
    Pulsar(PulsarBus),
    Nats(crate::nats_bus::NatsBus),
    Null,
}

impl EventBus {
    /// Build the best available bus: Pulsar > NATS > Null.
    pub async fn connect(
        gateway_name: &str,
        pulsar_url: Option<&str>,
        nats_url: &str,
    ) -> Self {
        // Try Pulsar first (if configured)
        if let Some(url) = pulsar_url {
            if let Some(bus) = PulsarBus::connect(url, gateway_name).await {
                return Self::Pulsar(bus);
            }
            warn!("Pulsar configured but unavailable — trying NATS fallback");
        }

        // Fall back to NATS
        if let Some(bus) = crate::nats_bus::NatsBus::connect(nats_url, gateway_name).await {
            return Self::Nats(bus);
        }

        warn!("No message bus available — event hooks will not fire");
        Self::Null
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Pulsar(_) => "pulsar",
            Self::Nats(_)   => "nats",
            Self::Null      => "none",
        }
    }

    pub async fn publish_snapshot(&self, project_id: &str, project_name: &str, snapshot_id: &str) {
        match self {
            Self::Pulsar(p) => p.publish_snapshot(project_id, project_name, snapshot_id).await,
            Self::Nats(n)   => n.publish_snapshot(project_id, project_name, snapshot_id).await,
            Self::Null      => {}
        }
    }

    pub async fn publish_task_log(&self, project_id: &str, head_cursor: i64) {
        match self {
            Self::Pulsar(p) => p.publish_task_log(project_id, head_cursor).await,
            Self::Nats(n)   => n.publish_task_log(project_id, head_cursor).await,
            Self::Null      => {}
        }
    }

    pub async fn publish_task_assigned(&self, agent_name: &str, project_id: &str, task: &str, todo_id: Option<i64>) {
        match self {
            Self::Pulsar(p) => p.publish_task_assigned(agent_name, project_id, task, todo_id).await,
            Self::Nats(n)   => n.publish_task_assigned(agent_name, project_id, task, todo_id).await,
            Self::Null      => {}
        }
    }

    pub async fn publish_online(&self, agent_count: usize) {
        match self {
            Self::Pulsar(p) => p.publish_online(agent_count).await,
            Self::Nats(n)   => n.publish_online(agent_count).await,
            Self::Null      => {}
        }
    }
}

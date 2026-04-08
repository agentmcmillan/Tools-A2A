/// Orchestrator agent — example agent using the a2a-sdk.
///
/// This agent receives tasks from the user (via the gateway CLI or API)
/// and fans them out to specialist agents (research, writer, etc.).
///
/// Soul: orchestrator role, can delegate to research and writer.
/// Memory: accumulates summaries of completed tasks.
/// Todos: tracks outstanding delegations.

use a2a_sdk::{Agent, Message, Response, TaskAssignedEvent, SnapshotChangedEvent};
use anyhow::Result;
use tracing::info;

const SOUL: &str = r#"
[agent]
name        = "orchestrator"
role        = "coordinator"
description = "Receives tasks, breaks them down, and delegates to specialist agents."

[capabilities]
orchestrate = "Plan and coordinate multi-step tasks"
delegate    = "Assign subtasks to the right specialist"
"#;

const GATEWAY: &str = "http://127.0.0.1:7240";
const ENDPOINT: &str = "http://orchestrator:8080";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("orchestrator=info".parse()?)
            .add_directive("info".parse()?))
        .init();

    let gateway = std::env::var("A2A_GATEWAY").unwrap_or_else(|_| GATEWAY.into());
    let endpoint = std::env::var("A2A_ENDPOINT").unwrap_or_else(|_| ENDPOINT.into());

    info!("orchestrator starting, gateway={gateway}");

    let agent = Agent::builder()
        .name("orchestrator")
        .version(env!("CARGO_PKG_VERSION"))
        .gateway(&gateway)
        .endpoint(&endpoint)
        .soul(SOUL)
        .capability("orchestrate", "Plan and coordinate multi-step tasks")
        .capability("delegate", "Assign subtasks to the right specialist")
        // ── Hooks: reactive event callbacks via NATS ─────────────────────────
        .on_task_assigned(|event: TaskAssignedEvent| async move {
            info!(task = %event.task, "hook: task assigned by reconciler");
            Ok(())
        })
        .on_snapshot_changed(|event: SnapshotChangedEvent| async move {
            info!(project = %event.project_id, snapshot = %event.snapshot_id, "hook: project snapshot changed");
            Ok(())
        })
        .build()
        .await?;

    // Append a startup todo so the reconciler picks it up
    agent.append_todo("Await first task from user").await?;

    info!("orchestrator registered, serving on {endpoint}");

    agent.serve(move |msg: Message| {
        async move {
            info!(method = %msg.method, caller = %msg.caller, "received task");

            // Parse incoming task
            let task = msg.payload.get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown task");

            // For now: echo back a plan skeleton
            // In production: delegate to research/writer and collect results
            Ok(Response::ok(serde_json::json!({
                "status": "accepted",
                "plan": format!("Orchestrating: {task}"),
                "steps": ["research", "draft", "review"]
            })))
        }
    }).await
}


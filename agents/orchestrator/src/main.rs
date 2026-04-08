/// Orchestrator agent — example agent using the a2a-sdk.
///
/// This agent receives tasks from the user (via the gateway CLI or API)
/// and fans them out to specialist agents (research, writer, etc.).
///
/// Soul: orchestrator role, can delegate to research and writer.
/// Memory: accumulates summaries of completed tasks.
/// Todos: tracks outstanding delegations.

use a2a_sdk::{Agent, Message, Response, TaskAssignedEvent, SnapshotChangedEvent};
use a2a_sdk::client::GatewayClient;
use anyhow::Result;
use serde_json::json;
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

/// Call the Anthropic Messages API with the given soul + todos to produce a PRD.
async fn generate_prd(gateway_addr: &str) -> Result<serde_json::Value> {
    // Connect to the gateway to fetch current agent state
    let mut gw = GatewayClient::connect(gateway_addr, "orchestrator").await?;

    let soul = gw.get_soul().await
        .map(|s| s.content)
        .unwrap_or_else(|_| "(no soul)".to_owned());

    let todos = gw.list_todos(false).await
        .unwrap_or_default();
    let todos_text = if todos.is_empty() {
        "(no todos)".to_owned()
    } else {
        todos.iter()
            .map(|t| format!("- [{}] {}", t.status, t.task))
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Check for API key
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return Ok(json!({"error": "ANTHROPIC_API_KEY not set"}));
    }

    info!("calling Anthropic API to generate PRD");

    let client = reqwest::Client::new();
    let resp = client.post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 4096,
            "messages": [{
                "role": "user",
                "content": format!(
                    "Generate a structured PRD (Product Requirements Document) from this agent state.\n\n\
                     ## Agent Soul\n{soul}\n\n\
                     ## Current Todos\n{todos_text}\n\n\
                     Output a markdown PRD with: Overview, Goals, Requirements, Success Metrics, Timeline."
                )
            }]
        }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or(json!({}));
            let prd = body["content"][0]["text"].as_str().unwrap_or("(no content)");
            // Store the generated PRD in agent memory
            gw.update_memory(&format!("## Generated PRD\n{prd}")).await.ok();
            Ok(json!({"prd": prd}))
        }
        Ok(r) => Ok(json!({"error": format!("API error: {}", r.status())})),
        Err(e) => Ok(json!({"error": format!("Request failed: {e}")})),
    }
}

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
        .capability("generate_prd", "Generate a PRD from current agent state via LLM")
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

    // Capture gateway address for use inside the handler
    let gateway_for_handler = gateway.clone();

    agent.serve(move |msg: Message| {
        let gw_addr = gateway_for_handler.clone();
        async move {
            info!(method = %msg.method, caller = %msg.caller, "received task");

            // ── PRD generation ─────────────────────────────────────────────
            if msg.method == "generate_prd" {
                let result = generate_prd(&gw_addr).await?;
                return Ok(Response::ok(result));
            }

            // ── Default: echo back a plan skeleton ─────────────────────────
            let task = msg.payload.get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown task");

            Ok(Response::ok(json!({
                "status": "accepted",
                "plan": format!("Orchestrating: {task}"),
                "steps": ["research", "draft", "review"]
            })))
        }
    }).await
}

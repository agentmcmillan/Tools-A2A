/// Writer agent — example agent using the a2a-sdk.
///
/// Accepts draft requests from the orchestrator and produces written content.
/// The writer can be asked to write, edit, or review text.

use a2a_sdk::{Agent, Message, Response};
use anyhow::Result;
use tracing::info;

const SOUL: &str = r#"
[agent]
name        = "writer"
role        = "writer"
description = "Drafts, edits, and reviews written content based on research inputs."

[capabilities]
write   = "Draft clear, well-structured written content"
edit    = "Review and improve existing drafts"
review  = "Provide constructive feedback on content quality"
"#;

const GATEWAY: &str = "http://127.0.0.1:7240";
const ENDPOINT: &str = "http://writer:8080";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("writer=info".parse()?)
            .add_directive("info".parse()?))
        .init();

    let gateway  = std::env::var("A2A_GATEWAY").unwrap_or_else(|_| GATEWAY.into());
    let endpoint = std::env::var("A2A_ENDPOINT").unwrap_or_else(|_| ENDPOINT.into());

    info!("writer agent starting, gateway={gateway}");

    let agent = Agent::builder()
        .name("writer")
        .version(env!("CARGO_PKG_VERSION"))
        .gateway(&gateway)
        .endpoint(&endpoint)
        .soul(SOUL)
        .capability("write",  "Draft clear, well-structured written content")
        .capability("edit",   "Review and improve existing drafts")
        .capability("review", "Provide constructive feedback on content quality")
        .build()
        .await?;

    info!("writer agent registered, serving on {endpoint}");

    agent.serve(move |msg: Message| {
        async move {
            let action = msg.payload.get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("write");

            match action {
                "write" => {
                    let topic = msg.payload.get("topic")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no topic)");
                    let context = msg.payload.get("context")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    info!(topic, caller = %msg.caller, "write request");

                    // Stub: in production, call an LLM with the topic + research context
                    Ok(Response::ok(serde_json::json!({
                        "action":     "write",
                        "topic":      topic,
                        "draft":      format!(
                            "## {topic}\n\n{}\n\n[stub — wire LLM here]",
                            if context.is_empty() { "No context provided." } else { context }
                        ),
                        "word_count": 10,
                    })))
                }

                "edit" => {
                    let draft = msg.payload.get("draft")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let instructions = msg.payload.get("instructions")
                        .and_then(|v| v.as_str())
                        .unwrap_or("improve clarity");

                    info!(instructions, caller = %msg.caller, "edit request");

                    Ok(Response::ok(serde_json::json!({
                        "action":  "edit",
                        "revised": format!("{draft}\n\n[edited per: {instructions} — stub]"),
                    })))
                }

                "review" => {
                    let draft = msg.payload.get("draft")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    info!(caller = %msg.caller, "review request");

                    Ok(Response::ok(serde_json::json!({
                        "action":   "review",
                        "score":    7,
                        "feedback": format!(
                            "Draft ({} chars) reviewed. [stub feedback — wire LLM here]",
                            draft.len()
                        ),
                        "approved": true,
                    })))
                }

                other => Ok(Response::ok(serde_json::json!({
                    "error": format!(
                        "unknown action '{other}' — supported: write, edit, review"
                    ),
                }))),
            }
        }
    }).await
}

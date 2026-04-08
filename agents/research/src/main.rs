/// Research agent — example agent using the a2a-sdk.
///
/// Accepts research queries from the orchestrator, performs (stub) research,
/// and returns a structured result.

use a2a_sdk::{Agent, Message, Response};
use anyhow::Result;
use tracing::info;

const SOUL: &str = r#"
[agent]
name        = "research"
role        = "researcher"
description = "Gathers information and synthesises research results on any topic."

[capabilities]
research    = "Search for information and summarise findings"
summarise   = "Condense large texts into key points"
"#;

const GATEWAY: &str = "http://127.0.0.1:7240";
const ENDPOINT: &str = "http://research:8080";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("research=info".parse()?)
            .add_directive("info".parse()?))
        .init();

    let gateway  = std::env::var("A2A_GATEWAY").unwrap_or_else(|_| GATEWAY.into());
    let endpoint = std::env::var("A2A_ENDPOINT").unwrap_or_else(|_| ENDPOINT.into());

    info!("research agent starting, gateway={gateway}");

    let agent = Agent::builder()
        .name("research")
        .version(env!("CARGO_PKG_VERSION"))
        .gateway(&gateway)
        .endpoint(&endpoint)
        .soul(SOUL)
        .capability("research", "Search for information and summarise findings")
        .capability("summarise", "Condense large texts into key points")
        .build()
        .await?;

    info!("research agent registered, serving on {endpoint}");

    agent.serve(move |msg: Message| {
        async move {
            let query = msg.payload.get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("(no query)");

            info!(query, caller = %msg.caller, "research request");

            // Stub: in production, call a web search API or RAG pipeline
            Ok(Response::ok(serde_json::json!({
                "query":   query,
                "summary": format!("Research findings for '{query}': [stub result — wire real search here]"),
                "sources": []
            })))
        }
    }).await
}


use crate::registry::Registry;
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HttpState {
    pub gateway_name: String,
    pub registry:     Registry,
    pub version:      String,
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/.well-known/agent.json", get(agent_card))
        .route("/health",                 get(health))
        .route("/",                       post(jsonrpc_dispatch))
        .with_state(state)
}

// ── Agent Card ────────────────────────────────────────────────────────────────

async fn agent_card(State(s): State<HttpState>) -> Json<Value> {
    let agents = s.registry.list();
    let capabilities: Vec<_> = agents.iter()
        .flat_map(|a| a.capabilities.iter().cloned())
        .collect();

    Json(json!({
        "schema_version": "0.3",
        "name":           s.gateway_name,
        "description":    "a2a-gateway — Rust-native agent-to-agent communication hub",
        "version":        s.version,
        "capabilities":   capabilities,
        "agents": agents.iter().map(|a| json!({
            "name":         a.name,
            "version":      a.version,
            "endpoint":     a.endpoint,
            "capabilities": a.capabilities,
        })).collect::<Vec<_>>(),
        "endpoints": {
            "jsonrpc": "/",
            "health":  "/health",
        }
    }))
}

// ── Health ────────────────────────────────────────────────────────────────────

async fn health(State(s): State<HttpState>) -> Json<Value> {
    let agents = s.registry.list();
    Json(json!({
        "ok":          true,
        "gateway":     s.gateway_name,
        "version":     s.version,
        "agent_count": agents.len(),
        "agents": agents.iter().map(|a| json!({
            "name":      a.name,
            "endpoint":  a.endpoint,
            "last_seen": a.last_seen,
        })).collect::<Vec<_>>(),
    }))
}

// ── A2A JSON-RPC 2.0 dispatcher ───────────────────────────────────────────────
//
// A2A spec v0.3 methods:
//   tasks/send   — submit a task to an agent
//   tasks/get    — get task status
//   tasks/cancel — cancel a running task

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method:  String,
    params:  Option<Value>,
    id:      Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result:  Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error:   Option<JsonRpcError>,
    id:      Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code:    i32,
    message: String,
}

impl JsonRpcResponse {
    fn ok(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0", result: Some(result), error: None, id }
    }
    fn err(id: Option<Value>, code: i32, msg: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            result:  None,
            error:   Some(JsonRpcError { code, message: msg.into() }),
            id,
        }
    }
}

async fn jsonrpc_dispatch(
    State(s): State<HttpState>,
    axum::extract::Json(req): axum::extract::Json<JsonRpcRequest>,
) -> (StatusCode, Json<JsonRpcResponse>) {
    if req.jsonrpc != "2.0" {
        return (
            StatusCode::BAD_REQUEST,
            Json(JsonRpcResponse::err(req.id, -32600, "invalid JSON-RPC version")),
        );
    }

    let id = req.id;
    match req.method.as_str() {
        "tasks/send" => {
            let params = req.params.unwrap_or(Value::Null);
            let agent  = params.get("agent").and_then(|v| v.as_str()).unwrap_or("");
            let task   = params.get("task").cloned().unwrap_or(Value::Null);

            if agent.is_empty() {
                return (StatusCode::OK, Json(JsonRpcResponse::err(id, -32602, "missing 'agent'")));
            }

            let agent_entry = match s.registry.get(agent) {
                Some(a) => a,
                None    => return (StatusCode::OK, Json(JsonRpcResponse::err(id, -32001, format!("agent '{agent}' not found")))),
            };

            // Forward to agent via HTTP
            let task_id = uuid::Uuid::new_v4().to_string();
            let client  = reqwest::Client::new();
            let payload = serde_json::to_vec(&task).unwrap_or_default();

            match client.post(format!("{}/invoke", agent_entry.endpoint))
                .body(payload)
                .header("content-type", "application/json")
                .send()
                .await
            {
                Ok(resp) => {
                    let result = resp.json::<Value>().await.unwrap_or(Value::Null);
                    (StatusCode::OK, Json(JsonRpcResponse::ok(id, json!({
                        "task_id": task_id,
                        "status": "completed",
                        "result": result,
                    }))))
                }
                Err(e) => (StatusCode::OK, Json(JsonRpcResponse::err(id, -32002, format!("agent call failed: {e}")))),
            }
        }

        "tasks/get" => {
            // Stub: task persistence is out of scope for Phase 2
            (StatusCode::OK, Json(JsonRpcResponse::err(id, -32001, "tasks/get not yet implemented")))
        }

        "tasks/cancel" => {
            (StatusCode::OK, Json(JsonRpcResponse::err(id, -32001, "tasks/cancel not yet implemented")))
        }

        other => (
            StatusCode::OK,
            Json(JsonRpcResponse::err(id, -32601, format!("method not found: {other}"))),
        ),
    }
}

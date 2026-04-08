/// Domain types for the a2a-sdk.
/// Proto types live in a2a-proto and never leak through the public SDK API.

use serde::{Deserialize, Serialize};

// ── Message ───────────────────────────────────────────────────────────────────

/// An inbound message delivered to an agent's handler.
#[derive(Debug, Clone)]
pub struct Message {
    /// Unique trace ID for this call (propagated from caller).
    pub trace_id: String,
    /// Name of the calling agent, or empty string for external callers.
    pub caller: String,
    /// Logical method/action the caller is requesting.
    pub method: String,
    /// JSON payload.
    pub payload: serde_json::Value,
}

// ── Response ──────────────────────────────────────────────────────────────────

/// A response returned from an agent's handler.
#[derive(Debug, Clone)]
pub struct Response {
    pub payload: serde_json::Value,
}

impl Response {
    pub fn ok(payload: serde_json::Value) -> Self {
        Self { payload }
    }
}

// ── Soul / Memory / Todo ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Soul {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id:           i64,
    pub task:         String,
    pub status:       String,
    pub created_at:   String,
    pub completed_at: Option<String>,
    pub notes:        Option<String>,
}

// ── Capability ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name:        String,
    pub description: String,
}

impl Capability {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into() }
    }
}

// ── StreamChunk ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub data:  bytes::Bytes,
    pub done:  bool,
}

/// Conversions between SDK domain types and a2a-proto generated types.
/// Proto types never appear in the public SDK API — all translation happens here.

use a2a_proto::local::{
    AgentManifest, CallRequest, SoulContent, MemoryPatch, TodoEntry, Todo as ProtoTodo,
};
use crate::types::{Capability, Message, Soul, TodoItem};
use uuid::Uuid;

// ── AgentManifest ─────────────────────────────────────────────────────────────

pub fn to_manifest(
    name: &str,
    version: &str,
    endpoint: &str,
    capabilities: &[Capability],
    soul_toml: &str,
) -> AgentManifest {
    AgentManifest {
        name:         name.to_owned(),
        version:      version.to_owned(),
        endpoint:     endpoint.to_owned(),
        capabilities: capabilities.iter().map(|c| c.name.clone()).collect(),
        soul_toml:    soul_toml.to_owned(),
    }
}

// ── CallRequest ───────────────────────────────────────────────────────────────

pub fn to_call_request(
    caller: &str,
    target: &str,
    method: &str,
    payload: &serde_json::Value,
) -> anyhow::Result<CallRequest> {
    Ok(CallRequest {
        target_agent: target.to_owned(),
        method:       method.to_owned(),
        payload:      serde_json::to_vec(payload)?,
        trace_id:     Uuid::new_v4().to_string(),
        caller:       caller.to_owned(),
        hop_count:    0, // gateway always overwrites this
    })
}

// ── Message (inbound) ─────────────────────────────────────────────────────────

pub fn from_call_request(req: &CallRequest) -> anyhow::Result<Message> {
    let payload: serde_json::Value = serde_json::from_slice(&req.payload)?;
    Ok(Message {
        trace_id: req.trace_id.clone(),
        caller:   req.caller.clone(),
        method:   req.method.clone(),
        payload,
    })
}

// ── Soul ──────────────────────────────────────────────────────────────────────

pub fn from_soul_content(sc: SoulContent) -> Soul {
    Soul { content: sc.content_toml }
}

pub fn to_memory_patch(agent_name: &str, content: &str) -> MemoryPatch {
    MemoryPatch {
        agent_name: agent_name.to_owned(),
        append_md:  content.to_owned(),
    }
}

pub fn to_todo_entry(agent_name: &str, task: &str) -> TodoEntry {
    TodoEntry {
        agent_name: agent_name.to_owned(),
        task:       task.to_owned(),
    }
}

// ── TodoItem ──────────────────────────────────────────────────────────────────

pub fn from_proto_todo(t: ProtoTodo) -> TodoItem {
    TodoItem {
        id:           t.id,
        task:         t.task,
        status:       t.status,
        created_at:   t.created_at,
        completed_at: if t.completed_at.is_empty() { None } else { Some(t.completed_at) },
        notes:        if t.notes.is_empty() { None } else { Some(t.notes) },
    }
}

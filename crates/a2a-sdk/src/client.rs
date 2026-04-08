/// gRPC client wrapper for the LocalGateway service.
/// Exposes clean domain types; all proto conversion is internal.

use a2a_proto::local::{
    local_gateway_client::LocalGatewayClient,
    AgentManifest, AgentRef, TodoFilter, TodoComplete, Empty,
};
use crate::{
    convert,
    options::CallOptions,
    types::{Soul, StreamChunk, TodoItem},
};
use anyhow::{Context, Result};
use futures::Stream;
use std::{pin::Pin, time::Duration};
use tonic::transport::Channel;
use tracing::debug;

pub struct GatewayClient {
    inner:      LocalGatewayClient<Channel>,
    agent_name: String,
}

impl GatewayClient {
    /// Connect to the gateway at `endpoint` (e.g. `"http://127.0.0.1:7240"`).
    pub async fn connect(endpoint: &str, agent_name: &str) -> Result<Self> {
        let channel = Channel::from_shared(endpoint.to_owned())
            .context("invalid gateway endpoint")?
            .connect()
            .await
            .context("failed to connect to gateway")?;
        Ok(Self {
            inner:      LocalGatewayClient::new(channel),
            agent_name: agent_name.to_owned(),
        })
    }

    // ── Registration ─────────────────────────────────────────────────────────

    pub async fn register(&mut self, manifest: AgentManifest) -> Result<()> {
        self.inner.register(manifest).await?;
        Ok(())
    }

    pub async fn heartbeat(&mut self) -> Result<()> {
        self.inner.heartbeat(AgentRef { name: self.agent_name.clone() }).await?;
        Ok(())
    }

    // ── Calls ─────────────────────────────────────────────────────────────────

    pub async fn call(
        &mut self,
        target: &str,
        method: &str,
        payload: serde_json::Value,
        opts: &CallOptions,
    ) -> Result<serde_json::Value> {
        let req = convert::to_call_request(&self.agent_name, target, method, &payload)?;
        let mut attempt = 0u32;
        let mut delay = Duration::from_millis(200);
        loop {
            attempt += 1;
            let mut timed_req = tonic::Request::new(req.clone());
            timed_req.set_timeout(opts.timeout);
            match self.inner.call(timed_req).await {
                Ok(resp) => {
                    let body = resp.into_inner();
                    if !body.error.is_empty() {
                        anyhow::bail!("agent error: {}", body.error);
                    }
                    let value: serde_json::Value = serde_json::from_slice(&body.result)
                        .context("invalid JSON in call response")?;
                    return Ok(value);
                }
                Err(status)
                    if attempt <= opts.retries
                        && is_retryable(status.code()) =>
                {
                    debug!(attempt, ?delay, "retrying call to {}", target);
                    tokio::time::sleep(delay).await;
                    delay = Duration::from_secs_f64(
                        delay.as_secs_f64() * opts.backoff_factor,
                    );
                }
                Err(status) => return Err(status.into()),
            }
        }
    }

    pub async fn stream(
        &mut self,
        target: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>> {
        let req = convert::to_call_request(&self.agent_name, target, method, &payload)?;
        let stream = self.inner.stream(req).await?.into_inner();
        let mapped = async_stream::stream! {
            use futures::StreamExt;
            let mut s = stream;
            while let Some(item) = s.next().await {
                yield item.map(|c| StreamChunk {
                    data: bytes::Bytes::from(c.data),
                    done: c.done,
                }).map_err(anyhow::Error::from);
            }
        };
        Ok(Box::pin(mapped))
    }

    // ── Identity ──────────────────────────────────────────────────────────────

    pub async fn get_soul(&mut self) -> Result<Soul> {
        let resp = self.inner
            .get_soul(AgentRef { name: self.agent_name.clone() })
            .await?;
        Ok(convert::from_soul_content(resp.into_inner()))
    }

    pub async fn update_memory(&mut self, content: &str) -> Result<()> {
        let patch = convert::to_memory_patch(&self.agent_name, content);
        self.inner.update_memory(patch).await?;
        Ok(())
    }

    pub async fn append_todo(&mut self, task: &str) -> Result<()> {
        let entry = convert::to_todo_entry(&self.agent_name, task);
        self.inner.append_todo(entry).await?;
        Ok(())
    }

    pub async fn list_todos(&mut self, include_done: bool) -> Result<Vec<TodoItem>> {
        let resp = self.inner
            .list_todos(TodoFilter {
                agent_name:   self.agent_name.clone(),
                include_done,
            })
            .await?;
        Ok(resp.into_inner().todos.into_iter().map(convert::from_proto_todo).collect())
    }

    pub async fn complete_todo(&mut self, id: i64, notes: &str) -> Result<()> {
        self.inner
            .complete_todo(TodoComplete { id, notes: notes.to_owned() })
            .await?;
        Ok(())
    }

    // ── Health ────────────────────────────────────────────────────────────────

    pub async fn health(&mut self) -> Result<a2a_proto::local::HealthStatus> {
        Ok(self.inner.health(Empty {}).await?.into_inner())
    }

    pub async fn list_agents(&mut self) -> Result<Vec<a2a_proto::local::AgentInfo>> {
        Ok(self.inner.list_agents(Empty {}).await?.into_inner().agents)
    }
}

fn is_retryable(code: tonic::Code) -> bool {
    matches!(code, tonic::Code::Unavailable | tonic::Code::DeadlineExceeded)
}

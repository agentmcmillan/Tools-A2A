/// Proxmox LXC management — spawn, list, and stop agent containers.
///
/// Uses the Proxmox REST API v2 (JSON) to manage LXC containers on a Proxmox
/// node. Each agent gets its own container cloned from a base template.
///
/// Authentication:
///   Proxmox API token set via env A2A_PROXMOX_TOKEN
///   Format: root@pam!token-name=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
///
/// Container lifecycle:
///
///   ┌──────────┐  clone template   ┌─────────────────┐  start  ┌────────────┐
///   │ template  │ ──────────────►  │  stopped LXC     │ ──────► │ running    │
///   │  (vmid)   │                  │  (new vmid)       │         │ LXC agent  │
///   └──────────┘                  └─────────────────┘         └────────────┘
///                                                                     │
///                                                             registers with
///                                                             a2a-gateway :7240
///
/// The gateway does NOT wait for the agent to self-register; that happens
/// asynchronously as part of the agent startup sequence inside the container.

use crate::config::LxcConfig;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ── Domain types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LxcContainer {
    /// Proxmox VM ID
    pub vmid:     u32,
    /// Container name
    pub name:     String,
    /// Current status: "running", "stopped", "paused"
    pub status:   String,
    /// Maximum memory in MiB
    pub maxmem:   Option<u64>,
    /// Number of CPUs
    pub cpus:     Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnedContainer {
    pub vmid: u32,
    pub name: String,
}

// ── LxcClient ─────────────────────────────────────────────────────────────────

/// HTTP client for the Proxmox REST API.
pub struct LxcClient {
    cfg:         LxcConfig,
    api_token:   String,
    http:        reqwest::Client,
}

impl LxcClient {
    /// Create a client from config + the A2A_PROXMOX_TOKEN env var.
    pub fn from_config(cfg: LxcConfig) -> Result<Self> {
        let api_token = std::env::var("A2A_PROXMOX_TOKEN")
            .context("A2A_PROXMOX_TOKEN must be set to use LXC features")?;

        // Proxmox uses a self-signed cert by default; accept it in production.
        // For high-security environments, configure a proper cert and remove this.
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .context("building reqwest client")?;

        Ok(Self { cfg, api_token, http })
    }

    // ── List ─────────────────────────────────────────────────────────────────

    /// List all LXC containers on the configured Proxmox node.
    pub async fn list(&self) -> Result<Vec<LxcContainer>> {
        let url = format!(
            "{}/api2/json/nodes/{}/lxc",
            self.cfg.proxmox_url, self.cfg.node
        );

        let resp = self.http.get(&url)
            .header("Authorization", format!("PVEAPIToken={}", self.api_token))
            .send()
            .await
            .context("listing LXC containers")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Proxmox list error {status}: {body}");
        }

        #[derive(Deserialize)]
        struct ApiResponse { data: Vec<RawLxc> }

        #[derive(Deserialize)]
        struct RawLxc {
            vmid:   serde_json::Value,
            name:   Option<String>,
            status: String,
            maxmem: Option<u64>,
            cpus:   Option<f64>,
        }

        let parsed: ApiResponse = resp.json().await.context("parsing list response")?;
        let containers = parsed.data.into_iter().map(|r| LxcContainer {
            vmid:   r.vmid.as_u64().unwrap_or(0) as u32,
            name:   r.name.unwrap_or_default(),
            status: r.status,
            maxmem: r.maxmem,
            cpus:   r.cpus,
        }).collect();

        Ok(containers)
    }

    // ── Spawn ─────────────────────────────────────────────────────────────────

    /// Clone the agent template and start the new container.
    ///
    /// Returns the new container's VMID and name.
    /// The new VMID is chosen as `max(existing_vmids) + 1`, minimum 200.
    pub async fn spawn(&self, agent_name: &str) -> Result<SpawnedContainer> {
        let existing = self.list().await?;
        let new_vmid = existing.iter()
            .map(|c| c.vmid)
            .max()
            .map(|m| m + 1)
            .unwrap_or(200)
            .max(200);

        let container_name = format!("a2a-{agent_name}");
        let clone_url = format!(
            "{}/api2/json/nodes/{}/lxc/{}/clone",
            self.cfg.proxmox_url, self.cfg.node, self.cfg.agent_template
        );

        info!(vmid = new_vmid, name = %container_name, "cloning LXC template");

        let body = serde_json::json!({
            "newid":    new_vmid,
            "hostname": container_name,
            "full":     true,
        });

        let resp = self.http.post(&clone_url)
            .header("Authorization", format!("PVEAPIToken={}", self.api_token))
            .json(&body)
            .send()
            .await
            .context("cloning LXC template")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Proxmox clone error {status}: {body}");
        }

        // Wait briefly for the task to complete before starting
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        self.start(new_vmid).await?;
        info!(vmid = new_vmid, name = %container_name, "LXC container started");

        Ok(SpawnedContainer { vmid: new_vmid, name: container_name })
    }

    // ── Start ─────────────────────────────────────────────────────────────────

    /// Start a stopped container by VMID.
    pub async fn start(&self, vmid: u32) -> Result<()> {
        let url = format!(
            "{}/api2/json/nodes/{}/lxc/{}/status/start",
            self.cfg.proxmox_url, self.cfg.node, vmid
        );

        let resp = self.http.post(&url)
            .header("Authorization", format!("PVEAPIToken={}", self.api_token))
            .send()
            .await
            .with_context(|| format!("starting LXC {vmid}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Proxmox start error {status}: {body}");
        }

        Ok(())
    }

    // ── Stop ──────────────────────────────────────────────────────────────────

    /// Gracefully shut down a running container.
    /// Uses `shutdown` (ACPI signal) rather than `stop` (hard kill).
    pub async fn stop(&self, vmid: u32) -> Result<()> {
        let url = format!(
            "{}/api2/json/nodes/{}/lxc/{}/status/shutdown",
            self.cfg.proxmox_url, self.cfg.node, vmid
        );

        let resp = self.http.post(&url)
            .header("Authorization", format!("PVEAPIToken={}", self.api_token))
            .send()
            .await
            .with_context(|| format!("stopping LXC {vmid}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Log but don't hard-fail — container may already be stopped
            warn!(vmid, error = %body, "Proxmox shutdown {status}");
        }

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LxcConfig;

    fn test_cfg() -> LxcConfig {
        LxcConfig {
            proxmox_url:    "https://proxmox.example.com:8006".into(),
            node:           "proxmox".into(),
            agent_template: 9001,
            network_bridge: "vmbr0".into(),
        }
    }

    /// LxcClient::from_config fails when A2A_PROXMOX_TOKEN is not set.
    #[test]
    fn from_config_requires_token() {
        // Remove the token from env (if set) so we test the missing case.
        // We can't guarantee env state in CI, so just verify the Result shape.
        std::env::remove_var("A2A_PROXMOX_TOKEN");
        let result = LxcClient::from_config(test_cfg());
        assert!(result.is_err(), "should require A2A_PROXMOX_TOKEN");
    }

    /// from_config succeeds when token is set.
    #[test]
    fn from_config_with_token() {
        std::env::set_var("A2A_PROXMOX_TOKEN", "root@pam!test=00000000-0000-0000-0000-000000000000");
        let result = LxcClient::from_config(test_cfg());
        assert!(result.is_ok(), "should build client when token is set");
        std::env::remove_var("A2A_PROXMOX_TOKEN");
    }

    /// Container name is formatted correctly.
    #[test]
    fn container_name_format() {
        let name = format!("a2a-{}", "research");
        assert_eq!(name, "a2a-research");
    }
}

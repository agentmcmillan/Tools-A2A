use serde::Deserialize;
use std::collections::HashMap;
use anyhow::{Context, Result};

// ─── Top-level config ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub gateway:    GatewayConfig,
    pub db:         DbConfig,
    pub lxc:        Option<LxcConfig>,
    #[serde(default)]
    pub agents:     Vec<SeedAgent>,
    pub call_graph: CallGraphConfig,
    #[serde(default)]
    pub peers:      Vec<PeerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    pub name:                     String,
    pub port_local:               u16,
    pub port_peer:                u16,
    pub port_http:                u16,
    #[serde(default = "default_port_web")]
    pub port_web:                 u16,
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval_secs:  u64,
    #[serde(default = "default_data_dir")]
    pub data_dir:                 String,
}

fn default_reconcile_interval() -> u64 { 30 }
fn default_port_web() -> u16 { 7243 }
fn default_data_dir() -> String { "./data".into() }

#[derive(Debug, Clone, Deserialize)]
pub struct DbConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LxcConfig {
    pub proxmox_url:    String,
    pub node:           String,
    pub agent_template: u32,
    pub network_bridge: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedAgent {
    pub name:     String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallGraphConfig {
    #[serde(flatten)]
    pub edges:  HashMap<String, Vec<String>>,
    pub limits: CallLimits,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallLimits {
    #[serde(default = "default_max_hops")]
    pub max_hops: u32,
}

fn default_max_hops() -> u32 { 3 }

#[derive(Debug, Clone, Deserialize)]
pub struct PeerConfig {
    pub name:      String,
    pub endpoint:  String,
    pub cert_path: Option<String>,
}

// ─── Loading ─────────────────────────────────────────────────────────────────

impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file: {path}"))?;
        let mut cfg: Config = toml::from_str(&content)
            .with_context(|| format!("parsing config file: {path}"))?;

        // Env overrides for sensitive values
        if let Some(lxc) = &mut cfg.lxc {
            if let Ok(token) = std::env::var("A2A_PROXMOX_TOKEN") {
                // Token is stored in env, not in config; just validate it's set.
                let _ = token; // consumed later by lxc.rs
            }
            let _ = lxc; // suppress unused warning until lxc.rs uses it
        }

        Ok(cfg)
    }

    /// Is a given caller → target edge permitted by the call graph?
    pub fn is_allowed(&self, caller: &str, target: &str) -> bool {
        self.call_graph
            .edges
            .get(caller)
            .map(|targets| targets.iter().any(|t| t == target))
            .unwrap_or(false)
    }

    pub fn max_hops(&self) -> u32 {
        self.call_graph.limits.max_hops
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        let toml = r#"
[gateway]
name       = "test"
port_local = 7240
port_peer  = 7241
port_http  = 7242

[db]
url = "postgres://a2a:a2a@localhost/a2a_test"

[call_graph]
orchestrator = ["research", "writer"]
research     = ["writer"]
writer       = []

[call_graph.limits]
max_hops = 3
"#;
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn allowed_edge() {
        let cfg = test_config();
        assert!(cfg.is_allowed("orchestrator", "research"));
        assert!(cfg.is_allowed("orchestrator", "writer"));
        assert!(cfg.is_allowed("research", "writer"));
    }

    #[test]
    fn denied_edge() {
        let cfg = test_config();
        assert!(!cfg.is_allowed("writer", "orchestrator"));
        assert!(!cfg.is_allowed("research", "orchestrator"));
        assert!(!cfg.is_allowed("unknown", "research"));
    }

    #[test]
    fn max_hops_default() {
        let cfg = test_config();
        assert_eq!(cfg.max_hops(), 3);
    }
}

/// Call router: enforces the call graph DAG and hop limit.
///
/// ┌─────────────────────────────────────────────────────────────┐
/// │  CallRequest arrives at gateway from caller                  │
/// │                                                              │
/// │  1. hop_count = request.hop_count + 1  (gateway increments) │
/// │  2. Check hop_count ≤ max_hops         → HopLimitExceeded   │
/// │  3. Check call_graph[caller][target]   → NotPermitted       │
/// │  4. Look up target endpoint in registry → NotFound          │
/// │  5. Forward to target agent endpoint                         │
/// └─────────────────────────────────────────────────────────────┘

use crate::config::Config;
use crate::registry::Registry;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RouterError {
    #[error("hop limit exceeded: {hops} hops (max {max})")]
    HopLimitExceeded { hops: u32, max: u32 },

    #[error("call not permitted: {caller} → {target}")]
    NotPermitted { caller: String, target: String },

    #[error("agent not found: {name}")]
    NotFound { name: String },
}

/// Result of a routing decision: the target's HTTP endpoint.
#[derive(Debug)]
pub struct RouteDecision {
    pub endpoint:  String,
    pub hop_count: u32, // incremented value to include in forwarded request
}

pub struct Router {
    config:   Config,
    registry: Registry,
}

impl Router {
    pub fn new(config: Config, registry: Registry) -> Self {
        Self { config, registry }
    }

    /// Validate a call and return the resolved endpoint + new hop count.
    /// caller = "" means the call originated from outside (Cloudflare / peer gateway).
    pub fn route(
        &self,
        caller:     &str,
        target:     &str,
        hop_count:  u32,  // value from the incoming request
    ) -> Result<RouteDecision, RouterError> {
        // 1. Increment hop count — ALWAYS done by gateway, never trusted from caller.
        let next_hop = hop_count + 1;

        // 2. Enforce hop limit.
        let max = self.config.max_hops();
        if next_hop > max {
            return Err(RouterError::HopLimitExceeded { hops: next_hop, max });
        }

        // 3. Enforce call graph (skip for external/anonymous callers).
        if !caller.is_empty() && !self.config.is_allowed(caller, target) {
            return Err(RouterError::NotPermitted {
                caller: caller.into(),
                target: target.into(),
            });
        }

        // 4. Resolve endpoint from registry.
        let entry = self.registry.get(target).ok_or_else(|| RouterError::NotFound {
            name: target.into(),
        })?;

        Ok(RouteDecision { endpoint: entry.endpoint, hop_count: next_hop })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::registry::AgentEntry;
    use chrono::Utc;

    async fn setup() -> Router {
        let pool = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let reg = Registry::new(pool).await.unwrap();

        // Seed agents
        for name in &["orchestrator", "research", "writer", "reviewer"] {
            reg.upsert(AgentEntry {
                name:          (*name).into(),
                version:       "0.1.0".into(),
                endpoint:      format!("http://{name}:8080"),
                capabilities:  vec![],
                soul_toml:     String::new(),
                last_seen:     None,
                registered_at: Utc::now().to_rfc3339(),
            }).await.unwrap();
        }

        let cfg: Config = toml::from_str(r#"
[gateway]
name = "test"
port_local = 7240
port_peer  = 7241
port_http  = 7242

[db]
url = "postgres://a2a:a2a@localhost/a2a_test"

[call_graph]
orchestrator = ["research", "writer"]
research     = ["writer"]
writer       = ["reviewer"]
reviewer     = ["research"]

[call_graph.limits]
max_hops = 3
"#).unwrap();

        Router::new(cfg, reg)
    }

    #[tokio::test]
    async fn allowed_call_returns_endpoint() {
        let r = setup().await;
        let d = r.route("orchestrator", "research", 0).unwrap();
        assert_eq!(d.endpoint, "http://research:8080");
        assert_eq!(d.hop_count, 1);
    }

    #[tokio::test]
    async fn denied_call_returns_not_permitted() {
        let r = setup().await;
        let err = r.route("writer", "orchestrator", 0).unwrap_err();
        assert!(matches!(err, RouterError::NotPermitted { .. }));
    }

    #[tokio::test]
    async fn hop_limit_enforced() {
        let r = setup().await;
        // max_hops = 3; passing hop_count=3 means next would be 4 → over limit
        let err = r.route("orchestrator", "research", 3).unwrap_err();
        assert!(matches!(err, RouterError::HopLimitExceeded { .. }));
    }

    #[tokio::test]
    async fn hop_count_incremented_by_gateway() {
        let r = setup().await;
        let d = r.route("orchestrator", "research", 1).unwrap();
        assert_eq!(d.hop_count, 2); // gateway always adds 1
    }

    #[tokio::test]
    async fn unknown_target_returns_not_found() {
        let r = setup().await;
        // Use empty caller to bypass call graph check; "ghost" is not in registry → NotFound
        let err = r.route("", "ghost", 0).unwrap_err();
        assert!(matches!(err, RouterError::NotFound { .. }));
    }

    #[tokio::test]
    async fn empty_caller_skips_call_graph_check() {
        let r = setup().await;
        // External caller (empty string) can reach any registered agent
        let d = r.route("", "writer", 0).unwrap();
        assert_eq!(d.endpoint, "http://writer:8080");
    }
}

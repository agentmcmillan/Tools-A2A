mod helpers;
use helpers::{TestGateway, config_with_call_graph};
use a2a_gateway::{
    config::Config,
    registry::Registry,
    router::{Router, RouterError},
    db,
};
use std::sync::Arc;

// Helper: build a Router with agents seeded from cfg.agents.
async fn make_router(cfg: Config) -> Router {
    use a2a_gateway::registry::AgentEntry;
    use chrono::Utc;

    let pool = db::open(":memory:").await.unwrap();
    let registry = Registry::new(pool).await.unwrap();

    for seed in &cfg.agents {
        registry.upsert(AgentEntry {
            name:          seed.name.clone(),
            version:       "test".into(),
            endpoint:      seed.endpoint.clone(),
            capabilities:  vec![],
            soul_toml:     String::new(),
            last_seen:     None,
            registered_at: Utc::now().to_rfc3339(),
        }).await.unwrap();
    }

    Router::new(cfg, registry)
}

/// Test: hop_count at max-1 succeeds (increments to exactly max).
#[tokio::test]
async fn hop_at_limit_minus_one_succeeds() {
    let cfg = config_with_call_graph(
        &[("a", "http://a:1"), ("b", "http://b:1")],
        &[("a", &["b"])],
        3,
    );
    let router = make_router(cfg).await;

    // hop_count=2 → gateway increments to 3 which equals max_hops → allowed
    let result = router.route("a", "b", 2);
    assert!(result.is_ok(), "hop at limit-1 must succeed, got {:?}", result);
    assert_eq!(result.unwrap().hop_count, 3);
}

/// Test: hop_count at max is rejected with HopLimitExceeded.
#[tokio::test]
async fn hop_at_limit_is_rejected() {
    let cfg = config_with_call_graph(
        &[("a", "http://a:1"), ("b", "http://b:1")],
        &[("a", &["b"])],
        3,
    );
    let router = make_router(cfg).await;

    // hop_count=3 → gateway would increment to 4 → exceeds max
    let result = router.route("a", "b", 3);
    assert!(matches!(result, Err(RouterError::HopLimitExceeded { .. })),
        "hop at max must be HopLimitExceeded, got {:?}", result);
}

/// Test: call_graph edge not configured returns NotPermitted.
#[tokio::test]
async fn missing_call_graph_edge_is_not_permitted() {
    let cfg = config_with_call_graph(
        &[("a", "http://a:1"), ("b", "http://b:1")],
        &[], // no edges configured
        3,
    );
    let router = make_router(cfg).await;

    let result = router.route("a", "b", 0);
    assert!(matches!(result, Err(RouterError::NotPermitted { .. })),
        "missing edge must be NotPermitted, got {:?}", result);
}

/// Test: unknown target agent returns NotFound.
#[tokio::test]
async fn unknown_target_returns_not_found() {
    let cfg = config_with_call_graph(
        &[("a", "http://a:1")], // "b" not registered
        &[("a", &["b"])],
        3,
    );
    let router = make_router(cfg).await;

    let result = router.route("a", "b", 0);
    assert!(matches!(result, Err(RouterError::NotFound { .. })),
        "unknown target must be NotFound, got {:?}", result);
}

/// Test: empty caller bypasses call graph check (external/anonymous caller).
#[tokio::test]
async fn empty_caller_bypasses_call_graph() {
    let cfg = config_with_call_graph(
        &[("target", "http://target:1")],
        &[], // no edges — but anonymous callers always pass
        3,
    );
    let router = make_router(cfg).await;

    let result = router.route("", "target", 0);
    assert!(result.is_ok(),
        "anonymous caller must bypass call graph, got {:?}", result);
}

/// Test: gateway always increments hop_count regardless of input value.
#[tokio::test]
async fn gateway_increments_hop_count() {
    let cfg = config_with_call_graph(
        &[("a", "http://a:1"), ("b", "http://b:1")],
        &[("a", &["b"])],
        10,
    );
    let router = make_router(cfg).await;

    for input_hop in [0u32, 1, 5] {
        let result = router.route("a", "b", input_hop).unwrap();
        assert_eq!(result.hop_count, input_hop + 1,
            "gateway must always increment hop by exactly 1 (input={})", input_hop);
    }
}

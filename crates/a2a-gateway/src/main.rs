use anyhow::Result;
use a2a_gateway::{
    auth::JwtAuth,
    config,
    db,
    file_sync::FileSyncEngine,
    groups::GroupStore,
    http_server::{HttpState, router as http_router},
    identity::IdentityStore,
    local_server::LocalGatewayHandler,
    lxc::LxcClient,
    pulsar_bus::EventBus,
    object_store::ObjectStore,
    peer_server::PeerGatewayHandler,
    peer_sync::PeerSyncCoordinator,
    reconciler::Reconciler,
    registry::{Registry, AgentEntry},
    router::Router,
    task_log::TaskLog,
    web,
    contributions::ContributionStore,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::transport::Server;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter, prelude::*};

#[tokio::main]
async fn main() -> Result<()> {
    // ── Logging ──────────────────────────────────────────────────────────────
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env()
            .add_directive("a2a_gateway=info".parse()?)
            .add_directive("info".parse()?))
        .with(fmt::layer().json())
        .init();

    // ── Config ───────────────────────────────────────────────────────────────
    let config_path = std::env::var("A2A_CONFIG").unwrap_or_else(|_| "gateway.toml".into());
    let cfg = config::Config::load(&config_path)?;
    info!(name = %cfg.gateway.name, "a2a-gateway starting");

    // ── Database ─────────────────────────────────────────────────────────────
    // DATABASE_URL env var takes precedence over gateway.toml [db].url
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| cfg.db.url.clone());
    let db = db::open(&db_url).await?;

    // ── Data dir ─────────────────────────────────────────────────────────────
    let data_dir = cfg.gateway.data_dir.clone();
    std::fs::create_dir_all(&data_dir)?;

    // ── Credentials from env ──────────────────────────────────────────────────
    let jwt_secret = std::env::var("A2A_JWT_SECRET")
        .expect("A2A_JWT_SECRET is required. Generate with: openssl rand -hex 32");
    let entropy_url = std::env::var("ENTROPY_WEBHOOK_URL").ok();
    let entropy_tok = std::env::var("ENTROPY_BEARER_TOKEN").ok();
    let nats_url    = std::env::var("NATS_URL")
        .unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let web_token   = std::env::var("A2A_WEB_TOKEN").ok();

    // ── Auth (JWT, used for peer boundary) ───────────────────────────────────
    let auth = JwtAuth::new(jwt_secret.as_bytes(), &cfg.gateway.name);

    // ── Registry ─────────────────────────────────────────────────────────────
    let registry = Registry::new(db.clone()).await?;
    for seed in &cfg.agents {
        use chrono::Utc;
        registry.upsert(AgentEntry {
            name:          seed.name.clone(),
            version:       "unknown".into(),
            endpoint:      seed.endpoint.clone(),
            capabilities:  vec![],
            soul_toml:     String::new(),
            last_seen:     None,
            registered_at: Utc::now().to_rfc3339(),
        }).await?;
    }

    // ── Router ───────────────────────────────────────────────────────────────
    let router = Arc::new(Router::new(cfg.clone(), registry.clone()));

    // ── Identity ─────────────────────────────────────────────────────────────
    let identity = IdentityStore::new(db.clone());

    // ── File sync + object store ──────────────────────────────────────────────
    let objects   = ObjectStore::new(db.clone(), format!("{data_dir}/objects"));
    let file_sync = Arc::new(FileSyncEngine::new(db.clone(), objects, &cfg.gateway.name));

    // ── Task log ──────────────────────────────────────────────────────────────
    let task_log = Arc::new(TaskLog::new(db.clone(), jwt_secret.as_bytes()));

    // ── Groups + contributions ────────────────────────────────────────────────
    let groups       = Arc::new(GroupStore::new(db.clone()));
    let _contributions = Arc::new(ContributionStore::new(db.clone()));

    // ── Event bus: Pulsar (preferred) > NATS (fallback) > Null (dev) ────────
    let pulsar_url = std::env::var("PULSAR_URL").ok();
    let bus = Arc::new(EventBus::connect(
        &cfg.gateway.name,
        pulsar_url.as_deref(),
        &nats_url,
    ).await);
    info!(bus = bus.name(), "event bus initialized");

    // ── Reconciler ───────────────────────────────────────────────────────────
    Reconciler::new(
        db.clone(),
        cfg.gateway.reconcile_interval_secs,
        &data_dir,
        bus.clone(),
        &cfg.gateway.name,
    ).spawn();

    // ── Peer sync coordinator ─────────────────────────────────────────────────
    if !cfg.peers.is_empty() {
        PeerSyncCoordinator::new(
            &cfg.gateway.name,
            cfg.peers.clone(),
            auth.clone(),
            file_sync.clone(),
            task_log.clone(),
            db.clone(),
            60,
        ).spawn();
    }

    // ── HTTP server :7242 (A2A spec) ──────────────────────────────────────────
    let http_state = HttpState {
        gateway_name: cfg.gateway.name.clone(),
        registry:     registry.clone(),
        version:      env!("CARGO_PKG_VERSION").to_owned(),
    };
    let http_addr: SocketAddr = format!("0.0.0.0:{}", cfg.gateway.port_http).parse()?;
    let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
    info!(%http_addr, "A2A HTTP listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(http_listener, http_router(http_state)).await {
            tracing::error!(error = %e, "A2A HTTP server crashed");
        }
    });

    // ── LXC client (optional) ─────────────────────────────────────────────────
    let lxc_client = cfg.lxc.clone().and_then(|lxc_cfg| {
        match LxcClient::from_config(lxc_cfg) {
            Ok(c)  => { info!("LXC client initialised"); Some(c) }
            Err(e) => { tracing::warn!(error=%e, "LXC disabled"); None }
        }
    });

    // ── Web UI :7243 ──────────────────────────────────────────────────────────
    let web_state = web::AppState::new(
        db.clone(),
        &cfg.gateway.name,
        jwt_secret.as_bytes(),
        entropy_url,
        entropy_tok,
        Some(registry.clone()),
        lxc_client,
        web_token,
    );
    let web_addr: SocketAddr = format!("0.0.0.0:{}", cfg.gateway.port_web).parse()?;
    let web_listener = tokio::net::TcpListener::bind(web_addr).await?;
    info!(%web_addr, "web UI listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(web_listener, web::router(web_state)).await {
            tracing::error!(error = %e, "web UI server crashed");
        }
    });

    // ── Peer gRPC :7241 ───────────────────────────────────────────────────────
    let peer_handler = PeerGatewayHandler::new(
        &cfg.gateway.name,
        auth.clone(),
        registry.clone(),
        router.clone(),
        file_sync.clone(),
        task_log.clone(),
        groups.clone(),
    );
    let peer_addr: SocketAddr = format!("0.0.0.0:{}", cfg.gateway.port_peer).parse()?;
    info!(%peer_addr, "peer gRPC listening");
    tokio::spawn(async move {
        if let Err(e) = Server::builder()
            .add_service(peer_handler.into_service())
            .serve(peer_addr)
            .await
        {
            tracing::error!(error = %e, "peer gRPC server crashed");
        }
    });

    // ── Local gRPC :7240 (blocking — main task) ───────────────────────────────
    let local_addr: SocketAddr = format!("0.0.0.0:{}", cfg.gateway.port_local).parse()?;
    let handler = LocalGatewayHandler::new(registry.clone(), identity, router.clone(), &cfg.gateway.name);
    info!(%local_addr, "local gRPC listening");
    Server::builder()
        .add_service(handler.into_service())
        .serve(local_addr)
        .await?;

    Ok(())
}

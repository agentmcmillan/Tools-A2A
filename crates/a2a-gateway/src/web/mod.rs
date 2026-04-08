/// Web UI — Axum router served on :7243.
///
/// Routes:
///   GET  /                         → redirect to /dashboard
///   GET  /dashboard                → master project list
///   GET  /projects/:id             → project detail (status, task log, agents, contributions)
///   GET  /projects/:id/onboard     → htmx partial: pending onboarding prompt
///   POST /projects/:id/onboard     → submit onboarding answer
///   GET  /projects/:id/tasks       → htmx partial: task log (cursor-based polling)
///   POST /projects/:id/contributions → open a new contribution proposal
///   GET  /projects/:id/contributions/:prop_id → contribution detail + vote/review form
///   POST /projects/:id/contributions/:prop_id/vote  → cast agent vote
///   POST /projects/:id/contributions/:prop_id/review → accept/reject PR
///   GET  /lxc                      → JSON list of all LXC containers (requires [lxc] config)
///   POST /lxc/spawn                → clone agent template + start new container
///   POST /lxc/:vmid/stop           → graceful shutdown of a container
///   GET  /health                   → JSON health (same as :7242/health)

use crate::context_builder::ContextBuilder;
use crate::db::Db;
use crate::contributions::ContributionStore;
use crate::groups::GroupStore;
use crate::lxc::LxcClient;
use crate::onboarding::OnboardingManager;
use crate::projects::ProjectStore;
use crate::registry::Registry;
use crate::task_log::TaskLog;
use axum::{
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post}, Json, Router,
};
use minijinja::{Environment, Value};
use serde::Deserialize;
use std::sync::Arc;

pub mod projects;

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub projects:      ProjectStore,
    pub task_log:      Arc<TaskLog>,
    pub contributions: Arc<ContributionStore>,
    pub groups:        Arc<GroupStore>,
    pub onboarding:    Arc<OnboardingManager>,
    pub gateway_name:  String,
    pub env:           Arc<Environment<'static>>,
    /// Optional Proxmox LXC client — None when [lxc] section absent from gateway.toml
    pub lxc:           Option<Arc<LxcClient>>,
    /// Optional bearer token for web dashboard auth.
    /// When Some, all routes (except /health) require `Authorization: Bearer <token>`
    /// or `?token=<token>` query parameter.
    pub web_token:     Option<String>,
}

impl AppState {
    pub fn new(
        db:           Db,
        gateway_name: impl Into<String>,
        jwt_secret:   impl AsRef<[u8]>,
        entropy_webhook: Option<String>,
        entropy_token:   Option<String>,
        registry:     Option<Registry>,
        lxc:          Option<LxcClient>,
        web_token:    Option<String>,
    ) -> Self {
        let gw   = gateway_name.into();
        let tl   = Arc::new(TaskLog::new(db.clone(), jwt_secret.as_ref()));
        let cs   = Arc::new(ContributionStore::new(db.clone()));
        let gs   = Arc::new(GroupStore::new(db.clone()));

        let mut om = OnboardingManager::new(
            db.clone(), &gw,
            jwt_secret.as_ref(),
            entropy_webhook,
            entropy_token,
        );
        if let Some(reg) = registry {
            let cb = ContextBuilder::new(db.clone(), reg, &gw, jwt_secret.as_ref());
            om = om.with_context_builder(cb);
        }
        let ob = Arc::new(om);

        let mut env = Environment::new();
        // Load templates from the embedded include_str! calls in templates module
        register_templates(&mut env);
        let env = Arc::new(env);

        Self {
            projects:      ProjectStore::new(db),
            task_log:      tl,
            contributions: cs,
            groups:        gs,
            onboarding:    ob,
            gateway_name:  gw,
            env,
            lxc:           lxc.map(Arc::new),
            web_token,
        }
    }

    pub fn render(&self, tmpl: &str, ctx: Value) -> Result<Html<String>, AppError> {
        let t = self.env.get_template(tmpl)
            .map_err(|e| AppError::Template(e.to_string()))?;
        let html = t.render(ctx)
            .map_err(|e| AppError::Template(e.to_string()))?;
        Ok(Html(html))
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router(state: AppState) -> Router {
    // Authenticated routes — guarded by auth_middleware when A2A_WEB_TOKEN is set.
    let protected = Router::new()
        .route("/",                  get(root_redirect))
        .route("/dashboard",         get(projects::dashboard))
        .route("/projects/:id",      get(projects::project_detail))
        .route("/projects/:id/onboard",
               get(projects::onboard_prompt).post(projects::onboard_answer))
        .route("/projects/:id/tasks", get(projects::task_log_partial))
        .route("/projects/:id/contributions",
               post(projects::open_contribution))
        .route("/projects/:id/contributions/:prop_id",
               get(projects::contribution_detail))
        .route("/projects/:id/contributions/:prop_id/vote",
               post(projects::cast_vote))
        .route("/projects/:id/contributions/:prop_id/review",
               post(projects::review_pr))
        .route("/lxc",               get(lxc_list))
        .route("/lxc/spawn",         post(lxc_spawn))
        .route("/lxc/:vmid/stop",    post(lxc_stop))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    // Public routes — no auth required.
    let public = Router::new()
        .route("/health", get(health));

    public.merge(protected).with_state(state)
}

// ── Auth middleware ───────────────────────────────────────────────────────────

/// Timing-safe string comparison. Does NOT leak length information —
/// XOR-folds both strings padded to the longer length.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let len = a.len().max(b.len());
    let mut diff = (a.len() != b.len()) as u8;
    for i in 0..len {
        let x = if i < a.len() { a[i] } else { 0 };
        let y = if i < b.len() { b[i] } else { 0xff };
        diff |= x ^ y;
    }
    diff == 0
}

/// Query parameter used for browser-friendly token auth (`?token=<token>`).
#[derive(Deserialize, Default)]
struct TokenQuery {
    token: Option<String>,
}

/// Middleware that enforces bearer-token auth on the web dashboard.
///
/// When `AppState::web_token` is `None`, all requests pass through (backwards compatible).
/// When set, the request must carry the token as either:
///   - `Authorization: Bearer <token>` header, OR
///   - `?token=<token>` query parameter (for browser access).
async fn auth_middleware(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // If no token configured, auth is disabled — pass through.
    let expected = match &state.web_token {
        Some(t) => t,
        None => return next.run(req).await,
    };

    // Try Authorization header first.
    let header_token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_owned());

    // Fall back to query parameter.
    let provided = header_token.or(query.token);

    match provided {
        Some(ref tok) if constant_time_eq(tok, expected) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "unauthorized",
                "message": "Missing or invalid bearer token. Supply via Authorization header or ?token= query parameter."
            })),
        )
            .into_response(),
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn root_redirect() -> Redirect {
    Redirect::permanent("/dashboard")
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "a2a-gateway-web" }))
}

// ── LXC handlers ──────────────────────────────────────────────────────────────

fn lxc_client(state: &AppState) -> Result<Arc<LxcClient>, AppError> {
    state.lxc.clone().ok_or_else(|| AppError::NotFound(
        "LXC not configured — add [lxc] section to gateway.toml".into()
    ))
}

#[derive(Deserialize)]
struct SpawnRequest { agent_name: String }

async fn lxc_list(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let client = lxc_client(&state)?;
    let containers = client.list().await.map_err(AppError::Internal)?;
    Ok(Json(serde_json::to_value(containers).unwrap_or(serde_json::Value::Array(vec![]))))
}

async fn lxc_spawn(
    State(state): State<AppState>,
    Json(body):   Json<SpawnRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let client = lxc_client(&state)?;
    let spawned = client.spawn(&body.agent_name).await.map_err(AppError::Internal)?;
    Ok(Json(serde_json::to_value(spawned).unwrap_or(serde_json::Value::Null)))
}

async fn lxc_stop(
    State(state): State<AppState>,
    Path(vmid):   Path<u32>,
) -> Result<Json<serde_json::Value>, AppError> {
    let client = lxc_client(&state)?;
    client.stop(vmid).await.map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "ok": true, "vmid": vmid })))
}

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    Template(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self { Self::Internal(e) }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
            Self::Template(msg) => (StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("template error: {msg}")).into_response(),
            Self::Internal(e)   => (StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("internal error: {e}")).into_response(),
        }
    }
}

// ── Template registration ─────────────────────────────────────────────────────

fn register_templates(env: &mut Environment<'static>) {
    env.add_template_owned(
        "base.html",
        include_str!("../../templates/base.html").to_owned(),
    ).unwrap();
    env.add_template_owned(
        "dashboard.html",
        include_str!("../../templates/dashboard.html").to_owned(),
    ).unwrap();
    env.add_template_owned(
        "project.html",
        include_str!("../../templates/project.html").to_owned(),
    ).unwrap();
    env.add_template_owned(
        "onboard_prompt.html",
        include_str!("../../templates/onboard_prompt.html").to_owned(),
    ).unwrap();
    env.add_template_owned(
        "task_log_partial.html",
        include_str!("../../templates/task_log_partial.html").to_owned(),
    ).unwrap();
    env.add_template_owned(
        "contribution.html",
        include_str!("../../templates/contribution.html").to_owned(),
    ).unwrap();
}

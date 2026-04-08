# TODOS — a2a-gateway deferred work

Items explicitly out of scope for Phase 1 + Phase 2, tracked here.
Each item has enough context to be picked up independently.

---

## TODO-1: Web Dashboard UI

**What:** Axum-served web UI on :7243 — full interactivity beyond the current htmx partials.
Currently the gateway serves basic dashboard, project detail, and onboarding pages.
Deferred: call graph visualisation, live reconciler log, agent soul/memory/todo editing.

**Why:** Makes the system legible to operators without needing the CLI or raw HTTP.

**Where to start:**
- `crates/a2a-gateway/src/web/mod.rs` — add routes
- `crates/a2a-gateway/src/web/projects.rs` — extend handlers
- Use htmx for live updates; all data is already available via existing stores

**Depends on:** Phase 1 + 2 complete ✅

---

## TODO-2: LLM PRD Generation

**What:** Orchestrator agent reads `requests.md` + all agent todos, calls an LLM API,
writes a structured `prd.md` to the gateway DB.

**Why:** Human-readable project state synthesised from machine-tracked tasks; closes
the loop between reconciler output and human understanding.

**Where to start:**
- `agents/orchestrator/src/main.rs` — add `GeneratePRD` action handler
- `crates/a2a-gateway/src/identity.rs` — `MemoryStore::update()` for prd.md storage
- Use `reqwest` to call Anthropic API; key from env `ANTHROPIC_API_KEY`

**Depends on:** Orchestrator agent + gateway identity DB ✅

---

## TODO-3: Phase 3 — LXC Spawn + Sidecar ✅ COMPLETE

**What:** Two components (both implemented):
1. `lxc.rs` — Proxmox API client (list/spawn/stop LXC containers for agent VMs)
2. `a2a-sidecar` — Unix socket gRPC server that proxies Python/non-Rust agents to the gateway

**Implemented:**
- `crates/a2a-gateway/src/lxc.rs` — Proxmox REST client (`list`, `spawn`, `start`, `stop`)
- `crates/a2a-sidecar/src/main.rs` — Transparent Unix socket → TCP gRPC proxy
- `crates/a2a-cli/src/main.rs` — `lxc list / spawn / stop` subcommands
- `crates/a2a-gateway/src/web/mod.rs` — REST routes `GET /lxc`, `POST /lxc/spawn`, `POST /lxc/:vmid/stop`
- `docker-compose.yml` — `sidecar` service + `sidecar-sock` named volume
- `Dockerfile` — `sidecar` runtime stage

**Config needed:** `A2A_PROXMOX_TOKEN` env var, `[lxc]` section in gateway.toml

---

## TODO-4: Task State Machine (10 states) ✅ COMPLETE

**Implemented:**
- Migration `007_task_status.sql`: added `task_status` column with CHECK constraint for 10 states
- `TaskStatus` enum in `task_log.rs` with `as_str()`, `from_str()`, `is_terminal()`
- `append_with_status()` method for explicit status + validity window
- `by_status()` query method for reconciler
- Also includes temporal validity (`valid_from`/`valid_to`) — see TODO-6

**Depends on:** Phase 2 task log ✅

---

## TODO-5: Error Classification (Transient / Permanent / RequiresHuman) ✅ COMPLETE

**Implemented:**
- `crates/a2a-gateway/src/error_class.rs` — `ErrorClass` enum with 3 variants
- `from_http_status()` — classifies HTTP status codes (503→Transient, 404→Permanent, 409→NeedsHuman)
- `from_grpc_code()` — classifies gRPC codes (Unavailable→Transient, NotFound→Permanent, FailedPrecondition→NeedsHuman)
- `from_reqwest_error()` — classifies network-level errors (timeout/connect→Transient)
- `should_retry()` — only Transient errors are retryable
- Unit tests for all classification paths

**Next:** Wire into `local_server.rs` call path to automatically log error class with task_log entries.

**Depends on:** TODO-4 ✅

---

## TODO-6: Temporal Validity Windows on Task Log + Memory ✅ COMPLETE

**Implemented** (combined with TODO-4 in migration 007):
- `valid_from` / `valid_to` columns added to `task_log`
- Index on `valid_to WHERE valid_to IS NOT NULL` for fast expiry queries
- `by_status()` automatically filters expired entries
- `append_with_status()` accepts optional `valid_to` parameter

**Depends on:** Phase 2 task log ✅

---

## TODO-7: Verbatim Memory Storage + Metadata-Filtered Retrieval ✅ COMPLETE

**Implemented:**
- Migration `008_memory_facts.sql`: `memory_facts` table with agent_name, project_id, topic, content, validity
- `memory_service.rs`: `MemoryService` with `store()`, `recall()`, `recall_all()`, `expire()`
- Indexes on agent_name, project_id, and valid_to for fast filtered retrieval
- Auto-filters expired entries on recall

**Depends on:** TODO-6 ✅

---

## TODO-8: Cloudflare Tunnel + agents.cubic.build ✅ COMPLETE

**Implemented:**
- `cloudflared` service added to `docker-compose.yml` (profile: `tunnel`)
- Start with: `docker compose --profile tunnel up -d`
- `CF_TUNNEL_TOKEN` env var documented in `.env.example`
- Uses `cloudflare/cloudflared:2024.4.1` pinned image

**Config needed:** `CF_TUNNEL_TOKEN` env var (create tunnel first: `cloudflared tunnel create a2a-gateway`)

---

<!-- TODO-9 removed: a2a-sidecar fully implemented in Phase 3 (see TODO-3) -->

---

## TODO-10: LAN Peer Mode (plain gRPC) ✅ COMPLETE

**Implemented:**
- `config.rs`: added `tls: bool` field to `PeerConfig` (default: true)
- `peer_client.rs`: `connect()` accepts `use_tls` parameter; logs LAN mode when false
- `peer_sync.rs`: passes `peer.tls` flag to `PeerClient::connect()`
- LAN peers use `tls = false` in `gateway.toml`:
  ```toml
  [[peers]]
  name     = "hub"
  endpoint = "http://nas:7241"
  tls      = false
  ```
- JWT still required for all peer operations regardless of TLS setting

---

## TODO-11: Web Dashboard Auth ✅ COMPLETE

**Implemented:**
- `web/mod.rs`: Bearer token auth middleware via `A2A_WEB_TOKEN` env var
- Supports `Authorization: Bearer <token>` header OR `?token=<token>` query param
- `/health` endpoint bypasses auth (needed for Docker healthcheck)
- Constant-time token comparison to prevent timing attacks
- Backwards compatible: if `A2A_WEB_TOKEN` not set, auth is disabled
- `main.rs`: reads `A2A_WEB_TOKEN` env var and passes to `AppState`

## TODO-12: Security Hardening (Rate Limiting + CORS + Headers) ✅ COMPLETE

**Implemented:**
- `http_server.rs`: CORS layer (permissive for A2A interop)
- `http_server.rs`: Security headers middleware (X-Content-Type-Options, X-Frame-Options, X-XSS-Protection, Referrer-Policy)
- `nats_bus.rs`: NATS authentication support via `NATS_USER` / `NATS_PASSWORD` env vars
- `docker-compose.yml`: NATS server configured with `--user` / `--pass` flags
- `.env.example`: documented `NATS_USER`, `NATS_PASSWORD`, `ANTHROPIC_API_KEY`, `CF_TUNNEL_TOKEN`

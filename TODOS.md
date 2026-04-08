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

## TODO-7: Verbatim Memory Storage + Metadata-Filtered Retrieval

**What:** Store agent memory verbatim (do not summarise/compress at write time).
Add a `memory_facts` table with typed metadata columns (`project_id`, `agent_name`,
`topic`, `created_at`, `valid_to`) that enable filtered recall without vector search.

**Why:** Mempalace research shows verbatim storage achieves 96.6% LongMemEval accuracy
vs 84.2% for compressed summaries. The +34% accuracy boost from metadata filtering
over pure vector similarity means a well-indexed SQLite table outperforms a naive
embedding lookup for structured agent memory.

**Implementation notes:**
- The `memory_facts` table already exists in migrations; extend with typed columns
- `MemoryService` (not yet written) should store raw content + metadata
- Retrieval: SQL `WHERE project_id = ? AND agent_name = ? AND (valid_to IS NULL OR valid_to > ?)`
- Compression is for *display* only (e.g. the web UI summarises for the user), not for storage

**Where to start:**
- `crates/a2a-gateway/src/` — add `memory_service.rs` (new file)
- Wire into `identity.rs` `UpdateMemory` RPC
- Add to web UI: project memory page showing all facts for a project

**Depends on:** TODO-6 (validity windows) for expiry semantics

---

## TODO-8: Cloudflare Tunnel + agents.cubic.build

**What:** Deploy the gateway behind Cloudflare Tunnel at `agents.cubic.build` so
peer gateways can reach it over the public internet without port forwarding.

**Why:** Required for multi-site operation. The :7241 peer gRPC and :7242 HTTP
endpoints need to be reachable from `site-b`.

**Where to start:**
- `cloudflared tunnel create a2a-gateway`
- Add `cloudflared` service to `docker-compose.yml`
- Add `CF-Access-Client-Id` + `CF-Access-Client-Secret` header verification in `peer_server.rs`

**Config needed:** `CF_TUNNEL_TOKEN` env var

**Depends on:** Phase 2 peer server ✅, a running NAS deployment

---

<!-- TODO-9 removed: a2a-sidecar fully implemented in Phase 3 (see TODO-3) -->

---

## TODO-10: LAN Peer Mode (plain gRPC)

**What:** Allow `[[peers]]` entries to skip mTLS when both gateways are on the same LAN.
Currently the peer channel (:7241) always requires mTLS + JWT, even for two gateways
sitting on the same network segment.

**Why:** Multi-gateway LANs are a real topology (e.g. a dev gateway and a prod gateway
on the same network). Requiring self-signed certs for same-LAN peers is unnecessary
friction.

**Where to start:**
- `crates/a2a-gateway/src/config.rs` — add `tls: bool` (default true) to `PeerConfig`
- `crates/a2a-gateway/src/peer_client.rs` — connect with `Channel::from_shared()` (no TLS)
  when `peer.tls == false`
- `crates/a2a-gateway/src/peer_server.rs` — accept unauthenticated connections on a
  separate plain gRPC listener when `--lan-peers` flag is set
- Still require JWT for authorization (prevents random LAN hosts from delegating)

**Depends on:** Phase 2 peer server ✅

---

## TODO-11: Web Dashboard Auth

**What:** Add authentication to the :7243 web dashboard. Currently it's open HTTP
on the LAN with no login.

**Why:** Any device on the LAN can browse agent data, trigger onboarding, and view
contributions. Fine for a home lab, not acceptable for a shared network.

**Where to start:**
- Simple approach: HTTP Basic auth with password from `A2A_WEB_PASSWORD` env var
- Better approach: Passkey/WebAuthn (reuse entropy-reader's implementation)
- `crates/a2a-gateway/src/web/mod.rs` — add auth middleware layer

**Depends on:** Phase 2 web UI ✅

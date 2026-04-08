# Tools-A2A

**Rust-native agent-to-agent communication gateway implementing the [A2A protocol v0.3](https://google.github.io/A2A/).**

A zero-trust, federated gateway that lets AI agents discover each other, delegate tasks, sync project files, and carry persistent identity — across machines, across networks.

```
           Developer A                              Developer B
        +-----------------+                      +-----------------+
        | a2a-gateway     |                      | a2a-gateway     |
        | + postgres      |                      | + postgres      |
        | + local agents  |    peer gRPC (mTLS)  | + local agents  |
        | scans ~/code/*  | <------------------> | scans ~/code/*  |
        +-----------------+                      +-----------------+
                |                                        |
                +------ peer sync via hub gateway -------+
                                    |
                        +-----------+-----------+
                        |   NAS / Hub Gateway   |
                        |   (always-on peer)    |
                        |   NATS + Jaeger       |
                        +-----------------------+
                                    |
                            Cloudflare Tunnel
                                    |
                        +-----------+-----------+
                        |   Remote Collaborator |
                        +-----------------------+
```

---

## What It Does

| Feature | How It Works |
|---------|-------------|
| **Agent Registration** | Agents self-register via gRPC, announcing name, version, capabilities, and a personality ("soul") |
| **Call Routing** | Gateway enforces a DAG call graph (who can call whom) with a configurable hop limit (default: 3) |
| **Agent Identity** | Each agent has a persistent soul (TOML), append-only memory (Markdown), and tracked todos (never deleted) |
| **File Sync** | Content-addressed object store (SHA256, git-like layout). Snapshots, diffs, and delta transfers between gateways |
| **Groups & Visibility** | Projects have three visibility tiers: `lan-only` -> `group` -> `public`. Groups use single-use invite tokens (48h TTL) |
| **Contributions** | PR-style change proposals with voting (majority for 3+ gateways, PR review for 2) |
| **Task Reconciler** | Rules-based 30-second loop: assigns tasks by capability match, merges duplicates, flags conflicts. No LLM required |
| **Reactive Hooks** | Agents receive NATS events when tasks are assigned, snapshots change, or lifecycle events fire |
| **Peer Federation** | Gateways sync state over mTLS gRPC with JWT auth. Each gateway runs independently; syncs when connected |
| **LXC Spawning** | Spawn agent containers on Proxmox via REST API. Gateway manages the full lifecycle |
| **Error Classification** | Errors are classified as Transient (retry), Permanent (log), or NeedsHuman (escalate) |

---

## Architecture

```
                        INTERNET / WAN
                             |
                +-----------[FW]----------+
                |                         |
                v                         v
        +---------------+        +---------------+
        | :7242 HTTPS   |        | :7241 mTLS    |
        | A2A JSON-RPC  |        | Peer gRPC     |
        | Agent Card    |        | (JWT auth)    |
        +-------+-------+        +-------+-------+
                |                         |
                v                         v
        +---------------------------------------------+
        |              a2a-gateway                     |
        |                                              |
        |  Registry ---- DashMap + PostgreSQL          |
        |  Router ------ Call graph DAG + hop limit    |
        |  TaskLog ----- HMAC-SHA256 signed, append    |
        |  FileSyncEngine  Content-addressed objects   |
        |  GroupStore -- Invite tokens, membership     |
        |  Reconciler -- 30s loop, capability matching |
        |  Hooks ------- NATS pub/sub to agents        |
        +-----+------------------+--------------------+
              |                  |
              v                  v
        +----------+    +--------------------+
        | :7240    |    | PostgreSQL         |
        | LAN gRPC |    | Object Store (fs)  |
        | (agents) |    | NATS (optional)    |
        +----+-----+    +--------------------+
             |
        +----+-----+   +----------+   +----------+
        | orch     |   | research |   | sidecar  |
        | agent    |   | agent    |   | (Python) |
        +----------+   +----------+   +----------+
```

### Protocol Stack

```
Layer 3  A2A v0.3 JSON-RPC    :7242    (spec-compliant, external)
         gRPC fast-path        :7241    (peer gateways, mTLS + JWT)
Layer 2  MCP                            (agent-to-tool, unchanged)
Layer 1  gRPC plain            :7240    (agent-to-gateway, LAN)
Layer 0  NATS Core             :4222    (event fan-out, optional)
```

### Port Map

| Port | Protocol | Auth | Purpose |
|------|----------|------|---------|
| 7240 | gRPC (plain) | None (LAN trusted) | Agent registration, calls, identity |
| 7241 | gRPC (mTLS) | JWT (HS256, 1hr TTL) | Peer gateway sync, delegation |
| 7242 | HTTP | Bearer token (optional) | A2A spec JSON-RPC, Agent Card, `/health` |
| 7243 | HTTP | None (LAN only) | Web dashboard |

---

## Project Structure

```
tools-a2a/
+-- Cargo.toml                    # Workspace root
+-- proto/
|   +-- local.proto               # Agent <-> gateway gRPC
|   +-- peer.proto                # Gateway <-> gateway gRPC
+-- crates/
|   +-- a2a-proto/                # Tonic codegen (build.rs)
|   +-- a2a-gateway/              # Gateway binary (main crate)
|   |   +-- src/
|   |   |   +-- main.rs           # Entry point, server startup
|   |   |   +-- config.rs         # gateway.toml loader
|   |   |   +-- db.rs             # PostgreSQL pool + migrations
|   |   |   +-- registry.rs       # Agent cache (DashMap + Postgres)
|   |   |   +-- router.rs         # Call graph enforcement + hop limit
|   |   |   +-- local_server.rs   # :7240 gRPC (Register, Call, Stream)
|   |   |   +-- peer_server.rs    # :7241 mTLS gRPC
|   |   |   +-- http_server.rs    # :7242 A2A JSON-RPC + Agent Card
|   |   |   +-- web/              # :7243 Web dashboard (htmx)
|   |   |   +-- identity.rs       # Soul / memory / todo CRUD
|   |   |   +-- reconciler.rs     # 30s task reconciliation loop
|   |   |   +-- file_sync.rs      # Snapshot / diff / apply
|   |   |   +-- object_store.rs   # Content-addressed blob store
|   |   |   +-- projects.rs       # Project CRUD + visibility FSM
|   |   |   +-- groups.rs         # Groups + invite tokens
|   |   |   +-- contributions.rs  # PR-style change proposals
|   |   |   +-- task_log.rs       # HMAC-signed append-only log
|   |   |   +-- error_class.rs    # Transient/Permanent/NeedsHuman
|   |   |   +-- auth.rs           # JWT issue/validate
|   |   |   +-- nats_bus.rs       # NATS event publisher
|   |   |   +-- lxc.rs            # Proxmox LXC management
|   |   |   +-- peer_sync.rs      # Periodic peer synchronization
|   |   +-- migrations/           # 7 PostgreSQL migrations
|   |   +-- templates/            # Minijinja HTML templates
|   |   +-- tests/                # 10 integration tests
|   +-- a2a-sdk/                  # Agent SDK library
|   |   +-- src/
|   |   |   +-- agent.rs          # Agent builder + serve loop
|   |   |   +-- hooks.rs          # Reactive NATS event callbacks
|   |   |   +-- client.rs         # gRPC client with retry
|   |   |   +-- types.rs          # Domain types (Message, Response)
|   |   |   +-- options.rs        # CallOptions (timeout, retries)
|   +-- a2a-sidecar/              # Unix socket -> gRPC bridge
|   +-- a2a-cli/                  # Operator CLI
+-- agents/
|   +-- orchestrator/             # Example: task coordinator
|   +-- research/                 # Example: research agent
|   +-- writer/                   # Example: writing agent
+-- gateway.toml                  # Production config
+-- dev-gateway.toml              # Developer-local config
+-- docker-compose.yml            # Full stack (8 services)
+-- dev-compose.yml               # Developer stack (gateway + postgres)
+-- install.sh                    # One-command developer setup
+-- Dockerfile                    # Multi-stage (6 targets)
```

---

## Quick Start

### Option 1: Developer Install (recommended)

```bash
git clone https://github.com/agentmcmillan/Tools-A2A.git
cd Tools-A2A
./install.sh
```

This starts a local gateway + PostgreSQL via Docker. Then:

```bash
# Install the CLI
cargo install --path crates/a2a-cli

# Check health
a2a health

# Run an example agent
cargo run -p orchestrator
```

### Option 2: Full Stack (NAS / Server)

```bash
cp .env.example .env
# Edit .env: set POSTGRES_PASSWORD, A2A_JWT_SECRET

docker compose up -d

# Verify
curl http://localhost:7242/health | jq .
```

### Option 3: From Source

```bash
# Prerequisites: Rust 1.88+, PostgreSQL 16+, protoc

cargo build --workspace --release

# Start PostgreSQL, then:
export DATABASE_URL=postgres://a2a:secret@localhost/a2a
export A2A_JWT_SECRET=$(openssl rand -hex 32)
./target/release/a2a-gateway
```

---

## Writing an Agent

```rust
use a2a_sdk::{Agent, Message, Response, TaskAssignedEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent = Agent::builder()
        .name("my-agent")
        .gateway("http://localhost:7240")
        .endpoint("http://localhost:8080")
        .soul(r#"
            [agent]
            name = "my-agent"
            role = "researcher"
            [capabilities]
            search = "Search documents and summarize"
        "#)
        .capability("search", "Search documents and summarize")
        // Reactive hook: called when the reconciler assigns work
        .on_task_assigned(|event: TaskAssignedEvent| async move {
            println!("New task: {}", event.task);
            Ok(())
        })
        .build()
        .await?;

    // Append a todo (tracked by the gateway, never deleted)
    agent.append_todo("Ready for tasks").await?;

    // Call another agent
    let result = agent.call("writer", serde_json::json!({
        "task": "Draft a summary of quantum computing"
    })).await?;
    println!("Writer responded: {result}");

    // Serve incoming calls
    agent.serve(|msg: Message| async move {
        Ok(Response::ok(serde_json::json!({
            "status": "done",
            "result": format!("Processed: {}", msg.method)
        })))
    }).await
}
```

---

## CLI Reference

```bash
# Health & discovery
a2a health                          # Gateway status
a2a agents list                     # Registered agents

# Call an agent
a2a agents call research '{"query":"rust async"}'

# Agent identity
a2a identity soul orchestrator      # View soul (personality)
a2a identity todos research --all   # View todos (including done)

# Projects
a2a projects list                   # List all projects
a2a projects add my-app --repo https://github.com/you/my-app
a2a projects log my-app             # Task log entries

# Peer management
a2a peers add --name hub --endpoint http://nas:7241
a2a peers ping http://nas:7241
a2a peers list

# Groups
a2a groups create alpha-team
a2a groups invite alpha-team        # Generate invite token
a2a groups join http://nas:7241 <token>

# LXC (requires Proxmox)
a2a lxc list
a2a lxc spawn research
a2a lxc stop 121
```

---

## Configuration

### gateway.toml

```toml
[gateway]
name       = "site-a"
port_local = 7240       # Agent gRPC (plain, LAN)
port_peer  = 7241       # Peer gRPC (mTLS)
port_http  = 7242       # A2A spec HTTP
port_web   = 7243       # Web dashboard
data_dir   = "/data"    # Object store + reconciler output
reconcile_interval_secs = 30

[db]
url = "postgres://a2a:secret@localhost/a2a"

# Seed agents (auto-registered on startup)
[[agents]]
name     = "orchestrator"
endpoint = "http://orchestrator:8080"

# Call graph: who can call whom
[call_graph]
orchestrator = ["research", "writer"]
research     = ["writer"]

[call_graph.limits]
max_hops = 3

# Peer gateways
[[peers]]
name     = "site-b"
endpoint = "https://agents.site-b.example"
cert_path = "./certs/site-b.pem"
```

### Environment Variables

| Variable | Required | Purpose |
|----------|----------|---------|
| `DATABASE_URL` | Yes | PostgreSQL connection string (overrides `[db].url`) |
| `A2A_JWT_SECRET` | Yes | HS256 secret for peer JWT auth (`openssl rand -hex 32`) |
| `POSTGRES_PASSWORD` | Docker only | PostgreSQL password (used in compose) |
| `A2A_PROXMOX_TOKEN` | For LXC | Proxmox API token (`user@realm!name=uuid`) |
| `NATS_URL` | No | NATS server URL (default: `nats://127.0.0.1:4222`) |
| `A2A_CONFIG` | No | Config file path (default: `gateway.toml`) |
| `ENTROPY_WEBHOOK_URL` | No | Push notification webhook |

---

## Dependencies

### Runtime

| Dependency | Version | Purpose |
|------------|---------|---------|
| PostgreSQL | 16+ | All gateway state (agents, projects, groups, task log) |
| NATS | 2.10+ | Event fan-out for agent hooks (optional, graceful degradation) |
| Docker | 24+ | Container builds and compose orchestration |

### Rust Crates (key dependencies)

| Crate | Purpose |
|-------|---------|
| `tonic` 0.12 | gRPC server + client (with TLS support) |
| `axum` 0.7 | HTTP server (A2A JSON-RPC + web dashboard) |
| `sqlx` 0.8 | PostgreSQL driver (pure Rust, async, compile-time checked) |
| `async-nats` 0.38 | NATS client for event pub/sub |
| `rustls` 0.23 | TLS (pure Rust, no OpenSSL dependency) |
| `jsonwebtoken` 9 | JWT issue/validate for peer auth |
| `prost` 0.13 | Protobuf serialization |
| `dashmap` 6 | Concurrent hashmap for agent registry cache |
| `sha2` / `hmac` | Content addressing + task log signatures |
| `reqwest` 0.12 | HTTP client (agent call forwarding, Proxmox API) |
| `tracing` + `opentelemetry` | Structured logging + distributed tracing (Jaeger) |
| `clap` 4 | CLI argument parsing |
| `minijinja` 2 | HTML templates for web dashboard |

### Build Requirements

| Tool | Version | Purpose |
|------|---------|---------|
| Rust | 1.88+ | Compiler (edition 2024) |
| `protoc` | 3.x | Protobuf compiler (for tonic-build) |
| Docker | 24+ | Container builds |
| Docker Compose | v2 | Multi-service orchestration |

---

## Security Model

```
:7240 (LAN)         No TLS, no auth. Network boundary = trust boundary.
                     Gateway enforces call graph + hop limits.

:7241 (Peers)        mTLS + JWT (HS256, 1hr TTL).
                     Peer names verified on handshake.

:7242 (External)     HTTPS via reverse proxy (Cloudflare/Caddy).
                     Optional bearer token auth.

:7243 (Web UI)       LAN-only HTTP. Auth planned (see TODOS.md).
```

Key security properties:
- All SQL uses parameterized queries (sqlx) — no injection vectors
- HMAC-SHA256 signed task log entries — tamper-evident audit trail
- Invite tokens: 32 random bytes, single-use, 48h TTL, atomic consumption
- Agent endpoints validated against SSRF blocklist (metadata services, loopback)
- Object store hashes validated as hex (no path traversal)
- Non-root container user in Dockerfile
- Secrets from env vars only — never in config files or code

Full threat model and 21-finding audit: [`docs/SECURITY_AUDIT.md`](docs/SECURITY_AUDIT.md)

---

## Deployment Topologies

### Single Server (simplest)

```bash
docker compose up -d    # Gateway + Postgres + NATS + Jaeger + 3 agents
```

### Distributed (recommended for teams)

Each developer runs their own gateway. A shared NAS acts as the sync hub:

```bash
# On each developer machine:
./install.sh
a2a peers add --name hub --endpoint http://nas:7241

# On the NAS:
docker compose up -d
```

See the [Architecture Guide](docs/a2a-architecture.md) for detailed topology diagrams.

---

## Database Schema

7 migrations, all idempotent (`CREATE TABLE IF NOT EXISTS`):

| Migration | Tables |
|-----------|--------|
| 001_init | `agents`, `memories`, `todos`, `requests`, `project_todos` |
| 002_projects | `projects`, `agent_projects` |
| 003_file_sync | `objects`, `snapshots`, `project_heads` |
| 004_task_log | `task_log` (HMAC-signed, append-only) |
| 005_contributions | `contribution_proposals`, `contribution_votes` |
| 006_groups | `groups`, `group_members`, `invites`, `group_projects` |
| 007_task_status | Adds `task_status` (10-state FSM) + `valid_from`/`valid_to` columns |

---

## Agent Hooks (NATS Events)

Agents can register reactive callbacks that fire when events occur:

```rust
.on_task_assigned(|event| async move { /* reconciler assigned a task */ })
.on_snapshot_changed(|event| async move { /* project files changed */ })
.on_lifecycle(|event| async move { /* registered / heartbeat lost */ })
.on_broadcast(|event| async move { /* gateway-wide announcement */ })
```

NATS subjects:
```
a2a.agent.{name}.task_assigned     # Per-agent task notification
a2a.agent.{name}.snapshot          # Per-agent file change
a2a.agent.{name}.lifecycle         # Per-agent lifecycle
a2a.broadcast.>                    # Gateway-wide events
a2a.projects.{id}.snapshot         # Project snapshot (all subscribers)
a2a.projects.{id}.tasklog          # Task log update (all subscribers)
```

---

## Visibility & Groups

Projects move through three visibility tiers:

```
  lan-only ---------> group ----------> public
     <------------------------------------
       (one step at a time, project must be idle)
```

| Visibility | Who Can See | Who Can Sync |
|------------|------------|--------------|
| `lan-only` | This gateway only | This gateway only |
| `group` | All gateways in the group | Group members |
| `public` | Anyone (via :7242) | Anyone (contributions require voting) |

Group membership uses single-use invite tokens:
```bash
a2a groups create my-team                    # Create group
a2a groups invite my-team                    # Generate 48h token
# Send token to collaborator out-of-band
a2a groups join http://their-gw:7241 <token> # They join
```

---

## Contributing

1. Fork the repo
2. Create a feature branch
3. Run `cargo build --workspace` (must be zero warnings)
4. Run `cargo test --workspace` (requires PostgreSQL via `DATABASE_URL`)
5. Open a PR

---

## License

MIT

---

## Documentation

- [Architecture Guide](docs/a2a-architecture.md) — Full 17-section technical reference
- [Architecture PDF](docs/a2a-architecture.pdf) — Printable version
- [Security Audit](docs/SECURITY_AUDIT.md) — 21 findings with STRIDE threat model
- [TODOs](TODOS.md) — Tracked deferred work with context
- [A2A Protocol Spec](https://google.github.io/A2A/) — Google's A2A v0.3 specification

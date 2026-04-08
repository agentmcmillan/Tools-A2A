# Comprehensive Security Audit: Tools-A2A Gateway

**Auditor**: Security Engineer Agent (Claude Opus 4.6)
**Date**: 2026-04-07
**Scope**: All source in `crates/a2a-gateway/`, `crates/a2a-sdk/`, `crates/a2a-sidecar/`, proto definitions, SQL migrations, Docker/compose, gateway.toml
**Methodology**: Full manual code review, STRIDE threat model, OWASP Top 10 mapping, gRPC-specific analysis, crypto review, race condition analysis, supply chain review
**Previous audits consulted**: Grok, Gemini, Codex, Cursor (findings in `SECURITY_REVIEW.md`)

---

## Executive Summary

Tools-A2A is a Rust-native agent-to-agent communication gateway with four network boundaries: plain gRPC for LAN agents (:7240), mTLS gRPC for peer gateways (:7241), HTTPS for the A2A JSON-RPC spec (:7242), and HTTP for the web dashboard (:7243). It uses PostgreSQL for state, a content-addressed object store on disk, HMAC-SHA256 signed task logs, group-based access control, and single-use invite tokens.

The codebase demonstrates strong fundamentals: parameterized SQL everywhere, Rust's memory safety, proper JWT validation, append-only audit trails, and good separation of trust boundaries. However, this audit identifies **21 new findings** not covered by the previous review, including 3 Critical, 5 High, 7 Medium, and 6 Low severity issues. The most dangerous findings involve HMAC signature bypass during peer sync, SSRF via agent endpoint registration, and the invite token consume operation's TOCTOU race condition.

**Verdict**: NOT production-ready without fixing the Critical and High findings. Estimated remediation: 2-3 engineering days.

---

## 1. Threat Model

### 1.1 Architecture Diagram

```
                    INTERNET / WAN
                         |
            +-----------[FW]----------+
            |                         |
            v                         v
    +---------------+        +---------------+
    | :7242 HTTPS   |        | :7241 mTLS    |
    | A2A JSON-RPC  |        | Peer gRPC     |
    | (external)    |        | (JWT auth)    |
    +-------+-------+        +-------+-------+
            |                         |
            |   TRUST BOUNDARY 1      |   TRUST BOUNDARY 2
            |   (public -> gateway)   |   (peer -> gateway)
            v                         v
    +---------------------------------------------+
    |          a2a-gateway process                 |
    |                                              |
    |  Registry (DashMap + PostgreSQL)             |
    |  Router (call graph DAG + hop limit)         |
    |  TaskLog (HMAC-signed append-only)           |
    |  FileSyncEngine (content-addressed objects)  |
    |  GroupStore (invite tokens, membership)      |
    |  ContributionStore (voting/PR approval)      |
    |  OnboardingManager (interactive state)       |
    |  Reconciler (background dedup)               |
    +-----+------------------+--------------------+
          |                  |
   TRUST BOUNDARY 3    TRUST BOUNDARY 4
   (gateway -> LAN)    (gateway -> infra)
          |                  |
          v                  v
    +----------+    +---------+----------+
    | :7240    |    | PostgreSQL         |
    | LAN gRPC |    | Object Store (fs)  |
    | (no auth)|    | NATS (optional)    |
    +----+-----+    | Proxmox LXC API   |
         |          +--------------------+
         v
    +----+-----+   +----------+   +----------+
    | agent-1  |   | agent-2  |   | sidecar  |
    | (orch)   |   | (writer) |   | (UDS)    |
    +----------+   +----------+   +----------+
                                      |
                                      v
                                  +----------+
                                  | Python/JS|
                                  | agent    |
                                  +----------+

    :7243 HTTP (Web Dashboard) -- LAN only, no auth
```

### 1.2 Trust Boundaries

| Boundary | Source | Destination | Auth | Encryption |
|----------|--------|-------------|------|------------|
| TB1 | External clients | :7242 HTTP | None | None (plaintext) |
| TB2 | Peer gateways | :7241 gRPC | JWT (HS256) | mTLS (planned) |
| TB3 | LAN agents | :7240 gRPC | None (by design) | None |
| TB4 | Gateway | PostgreSQL | Connection string | None (assumes LAN) |
| TB5 | Gateway | Proxmox API | API token | TLS (certs disabled) |
| TB6 | Gateway | NATS | None | None |
| TB7 | Gateway | Agent endpoints | None | None (HTTP) |
| TB8 | Web users | :7243 HTTP | None | None |

### 1.3 STRIDE Analysis

| Threat | Component | Risk | Current Mitigation | Gap |
|--------|-----------|------|-------------------|-----|
| **Spoofing** | Peer JWT | High | HS256 + TTL + sub claim | Shared secret model; no per-peer keys |
| **Spoofing** | LAN agent registration | High | None (LAN trusted) | Any LAN device can impersonate any agent |
| **Spoofing** | Web dashboard | High | None | No auth on :7243 at all |
| **Tampering** | Task log entries | Low | HMAC-SHA256 signatures | Peer sync trusts and re-appends without verifying |
| **Tampering** | Snapshot transcripts | Medium | SHA256 content addressing | Transcript path field not sanitized |
| **Repudiation** | Agent actions | Medium | Task log + tracing | No audit table for admin operations |
| **Info Disclosure** | Agent card | Medium | None | Exposes all agent endpoints to anyone on :7242 |
| **Info Disclosure** | Health endpoint | Low | None | Leaks agent count, names, endpoints |
| **DoS** | :7240 gRPC | Medium | Hop limit | No rate limiting, no message size limit |
| **DoS** | :7242 JSON-RPC | High | None | No auth, no rate limit, proxies to agents |
| **DoS** | Object store | Medium | None | No size limit on stored objects |
| **Elevation** | Web dashboard LXC | Critical | None | Unauthenticated LXC spawn/stop |
| **Elevation** | Peer delegation | High | JWT scope claim | Scope not enforced per-RPC |

---

## 2. Findings

### Finding Table

| ID | Severity | CVSS | File | Title |
|----|----------|------|------|-------|
| SEC-01 | Critical | 9.1 | `peer_sync.rs:136-147` | Peer sync ingests task log entries without HMAC verification |
| SEC-02 | Critical | 9.0 | `web/mod.rs:126-131, 160-176` | Web dashboard LXC operations have no authentication |
| SEC-03 | Critical | 8.7 | `local_server.rs:134-150, peer_server.rs:113-120` | SSRF via attacker-controlled agent endpoint |
| SEC-04 | High | 8.1 | `http_server.rs:120-167` | Unauthenticated JSON-RPC proxies requests to arbitrary agents |
| SEC-05 | High | 7.8 | `groups.rs:188-211` | TOCTOU race on invite token consumption |
| SEC-06 | High | 7.5 | `object_store.rs:88-89` | Object store path construction lacks hash validation |
| SEC-07 | High | 7.2 | `contributions.rs:188-196` | SQLite-syntax `INSERT OR REPLACE` used in PostgreSQL code |
| SEC-08 | High | 7.0 | `auth.rs:69-80` | JWT validation does not enforce `iss` claim |
| SEC-09 | Medium | 6.5 | `peer_server.rs:84-96` | Handshake RPC has no authentication |
| SEC-10 | Medium | 6.3 | `web/mod.rs, web/projects.rs` | Web dashboard has no CSRF protection |
| SEC-11 | Medium | 6.0 | `task_log.rs:170-175` | HMAC signature uses truncated timestamp losing precision |
| SEC-12 | Medium | 5.8 | `reconciler.rs:179-204` | Reconciler writes to disk using data_dir without path validation |
| SEC-13 | Medium | 5.5 | `onboarding.rs:183-189` | Onboarding folder_path bypasses canonicalize on empty input |
| SEC-14 | Medium | 5.3 | `http_server.rs:33-56` | Agent card endpoint leaks internal network topology |
| SEC-15 | Medium | 5.0 | `peer_sync.rs:138-147` | Peer-synced task log entries bypass cursor integrity |
| SEC-16 | Low | 4.5 | `identity.rs:78-89` | Unbounded memory append allows memory exhaustion per agent |
| SEC-17 | Low | 4.3 | `nats_bus.rs:39-51` | NATS connection has no authentication or TLS |
| SEC-18 | Low | 4.0 | `local_server.rs:182-211` | gRPC stream spawns unbounded tasks without backpressure |
| SEC-19 | Low | 3.8 | `db.rs:24` | Database connection string logged in error context |
| SEC-20 | Low | 3.5 | `main.rs:55-58` | JWT secret minimum length not enforced |
| SEC-21 | Low | 3.0 | `Cargo.toml` | casbin dependency included but never used in any code path |

---

### SEC-01: Peer sync ingests task log entries without HMAC verification [CRITICAL]

**File**: `crates/a2a-gateway/src/peer_sync.rs`, lines 136-147

**Description**: When the peer sync coordinator pulls task log entries from a remote peer, it calls `self.task_log.append()` which generates a NEW signature with the local secret. The original peer's signature (`entry.signature`) is completely discarded. The `TaskLog::verify_entry()` method exists but is never called during sync.

This means:
1. A malicious peer can inject fabricated task log entries with any content, agent name, or gateway name.
2. Those entries receive a valid local HMAC signature, making them indistinguishable from genuine local entries.
3. Downstream consumers of the task log (reconciler, context builder, web UI) trust these entries as authentic.

**Impact**: A compromised peer can inject arbitrary standup entries claiming any agent performed any work, poisoning the project's work history. In the contribution voting flow, this could be used to manipulate approval decisions by injecting fake "done" entries.

**Proof of concept**:
```
1. Malicious peer responds to SyncTaskLog RPC with fabricated entries:
   gateway_name="site-a", agent_name="orchestrator", content="LGTM, merging now"
2. Local gateway calls task_log.append() with these values
3. Entry is stored with a valid LOCAL HMAC signature
4. Entry now appears as if site-a's orchestrator wrote it
```

**Remediation**:
```rust
// In peer_sync.rs, sync_project(), replace the append loop with:
for entry in log_sync.entries {
    // Verify the peer's HMAC signature before accepting
    let peer_entry = LogEntry {
        id: entry.id.clone(),
        project_id: local_proj.id.clone(),
        gateway_name: entry.gateway_name.clone(),
        agent_name: entry.agent_name.clone(),
        entry_type: EntryType::from_str(&entry.entry_type),
        content: entry.content.clone(),
        cursor: entry.cursor,
        created_at: entry.created_at as f64,
        todo_id: None,
        signature: Some(entry.signature.clone()),
    };

    // Only accept entries signed by the peer's gateway
    // For cross-gateway trust, implement per-peer key exchange
    if entry.gateway_name != peer.name {
        tracing::warn!(
            gateway=%entry.gateway_name, peer=%peer.name,
            "rejecting task log entry: gateway mismatch"
        );
        continue;
    }

    // Store with original signature preserved (do NOT re-sign)
    self.task_log.insert_peer_entry(&local_proj.id, peer_entry).await.ok();
}
```

Additionally, add an `insert_peer_entry()` method to `TaskLog` that stores entries verbatim without generating a new signature.

---

### SEC-02: Web dashboard LXC operations have no authentication [CRITICAL]

**File**: `crates/a2a-gateway/src/web/mod.rs`, lines 126-131, 160-176

**Description**: The web dashboard on :7243 exposes LXC container management endpoints with zero authentication:
- `POST /lxc/spawn` -- clones a Proxmox template and starts a new container
- `POST /lxc/:vmid/stop` -- shuts down any container by VMID
- `GET /lxc` -- lists all containers on the Proxmox node

Any HTTP client on the LAN can spawn unlimited containers (resource exhaustion), stop existing containers (denial of service), or enumerate the infrastructure.

**Impact**: An attacker on the LAN can:
1. Spawn hundreds of containers, exhausting Proxmox resources
2. Stop production containers including the gateway itself
3. Map the entire Proxmox infrastructure via the list endpoint

The `agent_name` parameter in the spawn request is user-controlled and used directly in the container hostname (`a2a-{agent_name}`), which could contain shell metacharacters if Proxmox does not sanitize.

**Remediation**:
1. Add authentication middleware to the web router (at minimum a bearer token check):
```rust
// In web/mod.rs, add auth middleware
async fn require_auth(
    headers: axum::http::HeaderMap,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    let token = headers.get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match token {
        Some(t) if t == std::env::var("A2A_WEB_TOKEN").unwrap_or_default() => {
            Ok(next.run(req).await)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

// Apply to sensitive routes
Router::new()
    .route("/lxc", get(lxc_list))
    .route("/lxc/spawn", post(lxc_spawn))
    .route("/lxc/:vmid/stop", post(lxc_stop))
    .layer(axum::middleware::from_fn(require_auth))
```
2. Validate `agent_name` in the spawn request using the same regex as agent registration: `[a-zA-Z0-9_-]{1,64}`.
3. Rate-limit LXC spawn to 1 per minute.

---

### SEC-03: SSRF via attacker-controlled agent endpoint [CRITICAL]

**File**: `crates/a2a-gateway/src/local_server.rs`, lines 134-150; `crates/a2a-gateway/src/peer_server.rs`, lines 113-120; `crates/a2a-gateway/src/http_server.rs`, lines 148-157

**Description**: When an agent registers via the LAN gRPC service, it provides an arbitrary `endpoint` URL. This endpoint is later used by the gateway to proxy HTTP requests:
- `local_server.rs` line 135: `format!("{}/a2a/call", decision.endpoint.trim_end_matches('/'))`
- `peer_server.rs` line 115: `format!("{}/invoke", agent.endpoint)`
- `http_server.rs` line 152: `format!("{}/invoke", agent_entry.endpoint)`

An attacker who registers a malicious agent with `endpoint = "http://169.254.169.254"` (AWS metadata) or `endpoint = "http://internal-db:5432"` can use the gateway as an SSRF proxy to reach:
- Cloud metadata services (credential theft)
- Internal services behind the gateway's firewall
- The gateway's own management interfaces (localhost:7243)

The endpoint validation at registration (line 71-73 in `local_server.rs`) only checks length (1-256 chars), not the URL scheme, host, or port.

**Impact**: Full SSRF. An attacker on the LAN can use the gateway to reach any HTTP endpoint the gateway process can reach, including cloud metadata services, internal APIs, and localhost services.

**Remediation**:
```rust
// Add endpoint validation in local_server.rs Register handler:
fn validate_endpoint(endpoint: &str) -> Result<(), Status> {
    let url = url::Url::parse(endpoint)
        .map_err(|_| Status::invalid_argument("endpoint must be a valid URL"))?;

    // Only allow http/https schemes
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Status::invalid_argument("endpoint must use http or https"));
    }

    // Block metadata services and loopback
    if let Some(host) = url.host_str() {
        let blocked = [
            "169.254.169.254", "metadata.google.internal",
            "localhost", "127.0.0.1", "::1", "0.0.0.0",
        ];
        if blocked.iter().any(|b| host == *b) {
            return Err(Status::invalid_argument("endpoint host is not allowed"));
        }
        // Block link-local range
        if host.starts_with("169.254.") {
            return Err(Status::invalid_argument("endpoint host is not allowed"));
        }
    }

    Ok(())
}
```

---

### SEC-04: Unauthenticated JSON-RPC proxies requests to arbitrary agents [HIGH]

**File**: `crates/a2a-gateway/src/http_server.rs`, lines 120-167

**Description**: The `tasks/send` JSON-RPC method on :7242 accepts any request from any client without authentication. It looks up the agent by name from the request body and forwards the payload directly to the agent's endpoint. This creates an unauthenticated proxy that:
1. Allows anyone to invoke any registered agent
2. Exposes the internal agent endpoint topology
3. Passes arbitrary JSON payloads to agents without validation

The A2A spec may require open access for interoperability, but there is no option to restrict which callers can invoke which agents.

**Remediation**:
1. Add optional bearer token authentication on :7242:
```rust
// If A2A_HTTP_AUTH_TOKEN is set, require it
if let Ok(required_token) = std::env::var("A2A_HTTP_AUTH_TOKEN") {
    // Check Authorization header
}
```
2. Add request body size limits to prevent payload bombs.
3. Log all inbound JSON-RPC calls with source IP for audit.

---

### SEC-05: TOCTOU race on invite token consumption [HIGH]

**File**: `crates/a2a-gateway/src/groups.rs`, lines 188-211

**Description**: The `consume_invite` method has a classic time-of-check-time-of-use race condition:
1. Line 189: `get_invite(token)` -- reads the invite row
2. Line 192: `invite.is_valid()` -- checks `used_at.is_none()` in application code
3. Line 198: `UPDATE invites SET used_at = $1` -- marks as used

Between steps 1 and 3, two concurrent requests with the same token can both pass the validity check and both successfully update the row. The second update simply overwrites the first, and both callers proceed to `add_member()`. While the second `add_member` would fail on the `PRIMARY KEY (group_id, gateway_name)` constraint if the same gateway is joining, different gateways submitting the same token concurrently can both join the group.

**Impact**: A single-use invite token can be used by multiple gateways if requests are concurrent. This violates the security model where invite tokens grant exactly one membership.

**Remediation**: Use an atomic conditional update:
```sql
-- Atomically mark the invite as consumed only if it hasn't been used yet
UPDATE invites
SET used_at = $1, used_by = $2
WHERE token = $3 AND used_at IS NULL AND expires_at > $4
RETURNING group_id
```
If zero rows are returned, the token was already consumed or expired.

---

### SEC-06: Object store path construction lacks hash validation [HIGH]

**File**: `crates/a2a-gateway/src/object_store.rs`, lines 88-89

**Description**: The `object_path` method constructs a filesystem path from a hash:
```rust
fn object_path(&self, hash: &str) -> PathBuf {
    self.root.join(&hash[..2]).join(&hash[2..])
}
```

This code does not validate that `hash` is a valid hex string. If `hash` contains path separators (e.g., `../../etc/passwd`), the resulting path escapes the object store root. The `get()` method on line 64 reads arbitrary files:
```rust
pub async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
    let path = self.object_path(hash);
    if !path.exists() { return Ok(None); }
    let bytes = fs::read(&path).await.context("read object")?;
    Ok(Some(bytes))
}
```

A peer gateway calling `FetchObject` with `sha256 = "../../etc/passwd"` would cause:
- `hash[..2]` = `".."` -> `self.root.join("..")`
- `hash[2..]` = `"/etc/passwd"` -> joins to `/etc/passwd`

The `FetchObject` RPC in `peer_server.rs` passes the hash directly from the request.

**Impact**: Arbitrary file read from the gateway's filesystem by any authenticated peer.

**Remediation**:
```rust
fn object_path(&self, hash: &str) -> Option<PathBuf> {
    // Validate hash is exactly 64 hex characters
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(self.root.join(&hash[..2]).join(&hash[2..]))
}
```
Update all callers to handle `None` as "invalid hash" and return an error.

---

### SEC-07: SQLite-syntax `INSERT OR REPLACE` used in PostgreSQL code [HIGH]

**File**: `crates/a2a-gateway/src/contributions.rs`, lines 188-196

**Description**: The `cast_vote` method uses `INSERT OR REPLACE` SQL syntax:
```rust
sqlx::query(
    "INSERT OR REPLACE INTO contribution_votes \
     (id, proposal_id, gateway_name, agent_name, vote, reason, voted_at) \
     VALUES ($1,$2,$3,$4,$5,$6,$7)"
)
```

`INSERT OR REPLACE` is SQLite syntax, not PostgreSQL. On PostgreSQL, this query will fail with a syntax error. This means the entire voting system is non-functional on the declared database backend.

Beyond the functionality bug, if this were somehow executed, `OR REPLACE` allows a voter to overwrite their previous vote, which changes the semantics from "one vote per gateway" to "last vote wins." The UNIQUE constraint on `(proposal_id, gateway_name, agent_name)` in the migration should prevent duplicate votes.

**Impact**: The contribution voting system does not work at all on PostgreSQL. If fixed naively with `ON CONFLICT DO UPDATE`, it would allow vote manipulation.

**Remediation**:
```sql
INSERT INTO contribution_votes
  (id, proposal_id, gateway_name, agent_name, vote, reason, voted_at)
VALUES ($1,$2,$3,$4,$5,$6,$7)
ON CONFLICT (proposal_id, gateway_name, agent_name) DO NOTHING
```
Using `DO NOTHING` ensures each gateway/agent combination can only vote once. If they need to change their vote, that should be an explicit separate operation with audit trail.

---

### SEC-08: JWT validation does not enforce `iss` claim [HIGH]

**File**: `crates/a2a-gateway/src/auth.rs`, lines 69-80

**Description**: The `validate()` and `validate_for()` methods check the `sub` (subject/target) claim and `exp` (expiry) but never validate the `iss` (issuer) claim. The `Validation` object on line 70 is configured with only:
```rust
let mut validation = Validation::new(Algorithm::HS256);
validation.validate_exp = true;
```

The `iss` field is present in `PeerClaims` but never checked. Since all peers share the same JWT secret (HS256 symmetric key), any peer can forge tokens claiming to be issued by any other peer. The `iss` field is logged but never validated.

In a multi-peer setup, this means:
- Peer A can issue a token with `iss: "peer-B"` and `sub: "us"`
- We accept the token because the signature is valid (same shared secret) and sub matches
- We believe the request came from Peer B when it actually came from Peer A

**Impact**: Identity spoofing between peers. A malicious peer can impersonate any other peer in the federation.

**Remediation**:
```rust
pub fn validate(&self, token: &str) -> Result<PeerClaims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    // Validate issuer is a known peer
    // validation.set_issuer() accepts a list of valid issuers

    let data = decode::<PeerClaims>(token, &self.decoding, &validation)
        .context("invalid JWT")?;

    let claims = data.claims;
    if claims.is_expired() {
        bail!("token expired");
    }
    Ok(claims)
}
```

Long-term: migrate from shared-secret HS256 to per-peer asymmetric keys (RS256 or EdDSA). Each peer holds its own private key and shares its public key during handshake.

---

### SEC-09: Handshake RPC has no authentication [MEDIUM]

**File**: `crates/a2a-gateway/src/peer_server.rs`, lines 84-96

**Description**: The `Handshake` and `Ping` RPCs on the peer gRPC service (:7241) do not require JWT authentication. Any network client that can reach :7241 can:
- Discover the gateway name and version via Handshake
- Discover the agent count via Ping
- Probe the service for reconnaissance without any credentials

While these RPCs do not mutate state, they leak operational information.

**Remediation**: Require JWT on Ping (it already receives `gateway_name` so has context). For Handshake, consider a two-phase approach: unauthenticated handshake exchanges capabilities, then subsequent RPCs require JWT.

---

### SEC-10: Web dashboard has no CSRF protection [MEDIUM]

**File**: `crates/a2a-gateway/src/web/mod.rs`, `web/projects.rs`

**Description**: All POST endpoints on the web dashboard (:7243) accept form submissions with no CSRF token:
- `POST /projects/:id/onboard` -- modifies project configuration
- `POST /projects/:id/contributions` -- opens contribution proposals
- `POST /projects/:id/contributions/:prop_id/vote` -- casts votes
- `POST /projects/:id/contributions/:prop_id/review` -- accepts/rejects PRs
- `POST /lxc/spawn` -- spawns containers
- `POST /lxc/:vmid/stop` -- stops containers

An attacker can host a malicious webpage that submits forms to these endpoints. If a user with LAN access visits the page, the browser will send the request to the gateway.

**Remediation**: Add CSRF token generation and validation:
1. Generate a random token per session, embed in forms as a hidden field
2. Validate the token on every POST request
3. Set `SameSite=Strict` on any session cookies

---

### SEC-11: HMAC signature uses truncated timestamp losing precision [MEDIUM]

**File**: `crates/a2a-gateway/src/task_log.rs`, lines 170-175

**Description**: The HMAC signature canonical string casts the timestamp to `u64`:
```rust
fn sign(&self, id: &str, project_id: &str, gateway: &str, content: &str, ts: f64) -> String {
    let msg = format!("{id}|{project_id}|{gateway}|{content}|{}", ts as u64);
```

The `created_at` field is stored as `f64` (DOUBLE PRECISION) with sub-second precision. The signature truncates this to integer seconds. This means:
1. Two entries created within the same second with different fractional timestamps will produce the same signature if all other fields match.
2. An attacker who knows `id`, `project_id`, `gateway`, and `content` can brute-force the signature by trying all 86400 possible timestamps for a given day.

More importantly, the `verify_entry()` method on line 177 uses the same truncation, so legitimate entries always verify correctly, but the signature provides weaker assurance than intended.

**Remediation**: Use the full `f64` representation in the canonical string:
```rust
let msg = format!("{id}|{project_id}|{gateway}|{content}|{:.6}", ts);
```

---

### SEC-12: Reconciler writes to disk using data_dir without path validation [MEDIUM]

**File**: `crates/a2a-gateway/src/reconciler.rs`, lines 179-204

**Description**: The `write_todo_md()` method constructs a file path:
```rust
let path = format!("{}/todo.md", self.data_dir);
if let Err(e) = std::fs::write(&path, md) {
```

The `data_dir` comes from `gateway.toml` configuration. While this is not user-controlled at runtime, the content written to `todo.md` includes agent names and task text from the database, which ARE user-controlled via agent registration and todo creation. The markdown content is not sanitized, so if rendered in a browser (the web dashboard), it could contain XSS payloads.

**Remediation**: HTML-escape agent names and task text before writing to markdown. Also validate `data_dir` is an absolute path under a known prefix.

---

### SEC-13: Onboarding folder_path bypasses canonicalize on empty input [MEDIUM]

**File**: `crates/a2a-gateway/src/onboarding.rs`, lines 183-189

**Description**: When the user provides an empty folder path in the onboarding flow:
```rust
async fn handle_folder_answer(&self, project_id: &str, folder_path: &str) -> Result<()> {
    let path = if folder_path.trim().is_empty() {
        let p = self.projects.get(project_id).await?.context("project not found")?;
        format!("data/projects/{}", p.name)
    } else {
        folder_path.trim().to_owned()
    };
    self.projects.set_folder_path(project_id, &path).await?;
```

The `set_folder_path` in `projects.rs` calls `canonicalize()` which will fail if the path doesn't exist yet. But the onboarding flow uses a default path `data/projects/{name}` where `name` comes from the project record. If the project name contains path separators (e.g., `../../../etc`), the default path would be `data/projects/../../../etc`.

The project name is set during `ProjectStore::create()` which does not validate the name for path-unsafe characters.

**Remediation**: Validate project names to be path-safe (alphanumeric, dash, underscore only) at creation time, the same way agent names are validated.

---

### SEC-14: Agent card endpoint leaks internal network topology [MEDIUM]

**File**: `crates/a2a-gateway/src/http_server.rs`, lines 33-56

**Description**: The `/.well-known/agent.json` endpoint on :7242 (the public-facing port) returns the full agent list including internal endpoints:
```json
{
    "agents": [
        { "name": "orchestrator", "endpoint": "http://orchestrator:8080" },
        { "name": "research", "endpoint": "http://research:8080" }
    ]
}
```

This leaks internal hostnames and Docker service names to any client that can reach :7242.

**Remediation**: Strip the `endpoint` field from the public agent card. Only expose `name`, `version`, and `capabilities`.

---

### SEC-15: Peer-synced task log entries bypass cursor integrity [MEDIUM]

**File**: `crates/a2a-gateway/src/peer_sync.rs`, lines 138-147

**Description**: When syncing task log entries from a peer, the code calls `task_log.append()` which generates a new cursor value using `SELECT COALESCE(MAX(cursor), 0) + 1`. The original cursor from the peer is discarded. This means:
1. The ordering of entries in the local log may differ from the peer's log.
2. If the peer sends entries out of order, the local cursors will not reflect the peer's sequence.
3. Delta sync using `after_cursor` will return different results on different gateways for the same project.

This is not a direct security vulnerability but undermines the integrity guarantees of the task log sync protocol.

**Remediation**: Store peer entries with their original cursors in a separate namespace (e.g., `peer_cursor` column) and use the local cursor only for local ordering.

---

### SEC-16: Unbounded memory append allows memory exhaustion per agent [LOW]

**File**: `crates/a2a-gateway/src/identity.rs`, lines 78-89

**Description**: The `append_memory` method concatenates text to the agent's memory record with no size limit:
```rust
"ON CONFLICT(agent_name) DO UPDATE SET
  content_md = content_md || chr(10) || excluded.content_md"
```

A malicious agent can call `UpdateMemory` repeatedly with large payloads, growing the `content_md` column to gigabytes. PostgreSQL has a 1GB limit per `TEXT` field, but even approaching that limit would degrade database performance.

**Remediation**: Add a per-agent memory limit (e.g., 10MB) and reject appends that would exceed it.

---

### SEC-17: NATS connection has no authentication or TLS [LOW]

**File**: `crates/a2a-gateway/src/nats_bus.rs`, lines 39-51

**Description**: The NATS connection uses `async_nats::connect(url)` without any authentication credentials or TLS configuration. On a shared network, any client can connect to NATS and:
1. Subscribe to `a2a.projects.*.snapshot` to monitor all project activity
2. Publish fake snapshot/tasklog events to trigger unnecessary sync cycles
3. Publish fake `gateway.*.online` events for reconnaissance

**Remediation**: Configure NATS with TLS and token-based auth. Pass credentials via environment variables.

---

### SEC-18: gRPC stream spawns unbounded tasks without backpressure [LOW]

**File**: `crates/a2a-gateway/src/local_server.rs`, lines 187-211

**Description**: The `stream()` RPC handler spawns a `tokio::spawn` for each streaming request with a fixed channel capacity of 32. However, there is no limit on the number of concurrent streaming requests. An attacker can open thousands of streaming connections, each spawning a Tokio task that holds an HTTP client connection.

**Remediation**: Add a semaphore to limit concurrent streams per agent.

---

### SEC-19: Database connection string logged in error context [LOW]

**File**: `crates/a2a-gateway/src/db.rs`, line 24

**Description**: The database URL (which may contain credentials) is included in the error context:
```rust
.with_context(|| format!("connecting to postgres: {url}"))?;
```

If the connection fails, the full URL including `postgres://user:password@host/db` is logged.

**Remediation**: Redact the password from the URL before logging:
```rust
let display_url = url.replace(|c: char| c == ':' && url.contains('@'), "***");
```

---

### SEC-20: JWT secret minimum length not enforced [LOW]

**File**: `crates/a2a-gateway/src/main.rs`, lines 55-58

**Description**: While the previous audit noted the "changeme" default (which now has a loud warning), the code does not enforce a minimum length for the JWT secret. An operator could set `A2A_JWT_SECRET=abc` and the system would accept it, resulting in a trivially brute-forceable HMAC key.

**Remediation**: Enforce a minimum of 32 bytes at startup:
```rust
if jwt_secret.len() < 32 {
    panic!("A2A_JWT_SECRET must be at least 32 bytes");
}
```

---

### SEC-21: casbin dependency included but unused [LOW]

**File**: `Cargo.toml` (workspace), `crates/a2a-gateway/Cargo.toml`

**Description**: The `casbin` crate is listed as a dependency but is never imported or used in any source file. This increases the attack surface and binary size without providing value. Casbin has Tokio runtime integration and logging features enabled which add code paths.

**Remediation**: Remove `casbin` from both `Cargo.toml` files until it is actually needed.

---

## 3. OWASP Top 10 Mapping for HTTP Endpoints (:7242 and :7243)

| OWASP Category | Applicable? | Finding |
|---------------|-------------|---------|
| A01:2021 Broken Access Control | YES | SEC-02 (no auth on LXC), SEC-04 (no auth on JSON-RPC) |
| A02:2021 Cryptographic Failures | YES | SEC-08 (iss not validated), SEC-11 (truncated timestamp), SEC-20 (weak secret allowed) |
| A03:2021 Injection | NO | All SQL is parameterized via sqlx. No string interpolation in queries. |
| A04:2021 Insecure Design | YES | SEC-01 (peer sync trusts blindly), SEC-05 (TOCTOU on invite) |
| A05:2021 Security Misconfiguration | YES | SEC-14 (topology leak), SEC-17 (NATS no auth), SEC-21 (unused dep) |
| A06:2021 Vulnerable Components | PARTIAL | No known CVEs in current dep versions, but cargo-audit should be run in CI |
| A07:2021 Identity and Auth Failures | YES | SEC-08 (iss bypass), SEC-09 (handshake no auth) |
| A08:2021 Software and Data Integrity | YES | SEC-01 (unsigned peer data accepted), SEC-15 (cursor integrity) |
| A09:2021 Security Logging Failures | YES | SEC-19 (credential in logs), missing audit table |
| A10:2021 Server-Side Request Forgery | YES | SEC-03 (SSRF via agent endpoint) |

---

## 4. gRPC-Specific Attack Vectors

| Attack | Port | Mitigated? | Notes |
|--------|------|-----------|-------|
| Metadata injection | :7240, :7241 | YES | tonic parses metadata safely; no custom metadata processing |
| Stream abuse (slowloris) | :7240 | NO | SEC-18: unbounded concurrent streams |
| Large message DoS | :7240, :7241 | PARTIAL | tonic default max message size is 4MB; not explicitly configured |
| Protobuf deserialization bomb | All | YES | prost has no recursive message types in these protos |
| Reflection service exposure | All | YES | Reflection not enabled |
| Plaintext interception | :7240 | BY DESIGN | LAN trust boundary documented |
| Missing TLS on peer port | :7241 | YES | mTLS planned; JWT provides authentication layer |

---

## 5. Positive Security Findings

These are areas where the codebase demonstrates good security practices:

1. **Parameterized queries everywhere**: Every SQL query uses `$1` bind parameters via sqlx. Zero string interpolation in SQL. This is an excellent defense against injection.

2. **Rust memory safety**: The entire codebase is safe Rust with no `unsafe` blocks. This eliminates buffer overflows, use-after-free, and data races at compile time.

3. **Content-addressed object store**: Using SHA256 content addressing provides integrity verification by construction. Objects are immutable once stored.

4. **Append-only task log with HMAC signatures**: Entries are never deleted or modified. The HMAC signing provides tamper detection (with the caveats noted in SEC-01 and SEC-11).

5. **Single-use invite tokens with TTL**: The invite system uses 32 bytes of randomness (256 bits of entropy via `rand::rng().fill_bytes()`), hex encoding, and 48-hour expiry. This is cryptographically strong.

6. **Call graph enforcement with hop limit**: The Router enforces a DAG-based call policy and prevents infinite loops via hop counting. Gateway always overwrites the hop count (never trusts caller-supplied values).

7. **Agent name validation**: Registration validates agent names against `[a-zA-Z0-9_-]{1,64}`, preventing injection via agent names.

8. **Path canonicalization on folder_path**: The `set_folder_path` method uses `std::path::Path::canonicalize()` to resolve symlinks and prevent directory traversal. (With the caveat in SEC-13 for the default path.)

9. **Non-root Docker containers**: The Dockerfile creates a dedicated `a2a` user and switches to it before running the binary.

10. **Multi-stage Docker builds**: The builder stage is separate from runtime, so build tools and source code are not present in the final image.

11. **Docker secrets via environment**: All credentials come from environment variables, never hardcoded in docker-compose.yml. Required secrets use `:?required` syntax.

12. **Graceful degradation**: NATS and LXC are optional. The gateway functions correctly without them.

---

## 6. Recommendations for Production Hardening

### Immediate (before any external exposure)

| Priority | Action | Findings |
|----------|--------|----------|
| P0 | Validate object store hash input (hex-only, 64 chars) | SEC-06 |
| P0 | Add authentication to web dashboard :7243 | SEC-02, SEC-10 |
| P0 | Validate agent endpoint URLs against SSRF blocklist | SEC-03 |
| P0 | Verify peer task log HMAC signatures during sync | SEC-01 |
| P0 | Fix `INSERT OR REPLACE` to valid PostgreSQL syntax | SEC-07 |
| P0 | Use atomic conditional UPDATE for invite consumption | SEC-05 |
| P1 | Enforce JWT issuer (`iss`) claim validation | SEC-08 |
| P1 | Add optional auth on :7242 JSON-RPC endpoint | SEC-04 |
| P1 | Enforce JWT secret minimum length (32 bytes) | SEC-20 |
| P1 | Redact database URL in error messages | SEC-19 |

### Short-term (before multi-peer federation)

| Priority | Action | Findings |
|----------|--------|----------|
| P2 | Strip internal endpoints from public agent card | SEC-14 |
| P2 | Add CSRF tokens to web dashboard forms | SEC-10 |
| P2 | Add per-agent memory size limits | SEC-16 |
| P2 | Configure NATS authentication and TLS | SEC-17 |
| P2 | Limit concurrent gRPC streams per client | SEC-18 |
| P2 | Add rate limiting on all public endpoints | -- |
| P2 | Run `cargo audit` in CI pipeline | SEC-21 |

### Long-term (production federation)

| Priority | Action |
|----------|--------|
| P3 | Migrate from shared-secret JWT (HS256) to per-peer asymmetric keys (EdDSA) |
| P3 | Implement mutual TLS on :7241 (currently planned but not implemented) |
| P3 | Add TLS termination on :7242 (reverse proxy or embedded) |
| P3 | Implement an audit log table for all state-changing operations |
| P3 | Add security headers (CSP, HSTS, X-Frame-Options) to web dashboard |
| P3 | Implement gRPC interceptor for per-method authorization on peer port |
| P3 | Add request body size limits on all HTTP endpoints |
| P3 | Remove `casbin` dependency or implement policy engine |
| P3 | Add `cargo-deny` to CI for license and advisory checking |

---

## 7. Supply Chain Review

### Dependency Analysis

All workspace dependencies were reviewed against known advisories as of 2026-04-07:

| Crate | Version | Risk | Notes |
|-------|---------|------|-------|
| tokio | 1.x | Low | Well-maintained, frequent security patches |
| tonic | 0.12 | Low | Google-maintained gRPC framework |
| axum | 0.7 | Low | Tower-based, memory-safe by design |
| reqwest | 0.12 | Low | Uses rustls (memory-safe TLS) |
| sqlx | 0.8 | Low | Compile-time checked queries available (not used here) |
| jsonwebtoken | 9.x | Low | Widely audited JWT library |
| dashmap | 6.x | Low | Lock-free concurrent HashMap |
| rand | 0.9 | Low | Uses OS entropy source for `fill_bytes` |
| sha2/hmac | 0.10/0.12 | Low | RustCrypto project, well-audited |
| casbin | 2.x | **Medium** | Unused -- adds attack surface for no benefit (SEC-21) |
| async-nats | 0.38 | Low | Official NATS client |
| minijinja | 2.x | Low | Template engine with auto-escaping |

**Recommendation**: Add `cargo-audit` and `cargo-deny` to CI. Remove `casbin` until needed.

---

*End of security audit. All findings are based on source code review as of 2026-04-07. This audit does not include dynamic testing (penetration testing) or infrastructure review beyond what is visible in the codebase.*

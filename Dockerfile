# syntax=docker/dockerfile:1.7
# Multi-stage Dockerfile for the a2a workspace.
# Targets: gateway, orchestrator, research, writer, sidecar
#
# Build a specific target:
#   docker build --target gateway  -t a2a-gateway  .
#   docker build --target writer   -t a2a-writer   .
#   docker build --target sidecar  -t a2a-sidecar  .

# ── Build stage ───────────────────────────────────────────────────────────────

FROM rust:1.88-bookworm AS builder

# protoc required for tonic-build
# sqlx postgres uses a pure-Rust wire protocol — libpq is NOT required
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache workspace dependencies before copying source
COPY Cargo.toml Cargo.lock ./
COPY crates/a2a-proto/Cargo.toml   crates/a2a-proto/Cargo.toml
COPY crates/a2a-gateway/Cargo.toml crates/a2a-gateway/Cargo.toml
COPY crates/a2a-sdk/Cargo.toml     crates/a2a-sdk/Cargo.toml
COPY crates/a2a-sidecar/Cargo.toml crates/a2a-sidecar/Cargo.toml
COPY crates/a2a-cli/Cargo.toml     crates/a2a-cli/Cargo.toml
COPY agents/orchestrator/Cargo.toml agents/orchestrator/Cargo.toml
COPY agents/research/Cargo.toml    agents/research/Cargo.toml
COPY agents/writer/Cargo.toml      agents/writer/Cargo.toml

# Stub all lib/main entries so cargo can build deps without source
RUN mkdir -p crates/a2a-proto/src && echo "" > crates/a2a-proto/src/lib.rs && \
    mkdir -p crates/a2a-gateway/src && echo "fn main(){}" > crates/a2a-gateway/src/main.rs && \
    mkdir -p crates/a2a-sdk/src && echo "" > crates/a2a-sdk/src/lib.rs && \
    mkdir -p crates/a2a-sidecar/src && echo "fn main(){}" > crates/a2a-sidecar/src/main.rs && \
    mkdir -p crates/a2a-cli/src && echo "fn main(){}" > crates/a2a-cli/src/main.rs && \
    mkdir -p agents/orchestrator/src && echo "fn main(){}" > agents/orchestrator/src/main.rs && \
    mkdir -p agents/research/src && echo "fn main(){}" > agents/research/src/main.rs && \
    mkdir -p agents/writer/src && echo "fn main(){}" > agents/writer/src/main.rs

RUN cargo build --release 2>&1 | tail -5 || true

# Copy real source and proto definitions
COPY proto/ proto/
COPY crates/ crates/
COPY agents/ agents/

# Build all binaries
RUN cargo build --release -p a2a-gateway -p orchestrator -p research -p writer -p a2a-sidecar

# ── Runtime: gateway ──────────────────────────────────────────────────────────

FROM debian:bookworm-slim AS gateway

# curl: used by the docker-compose HEALTHCHECK (GET /health on :7242)
# ca-certificates: rustls needs the system trust store for outbound TLS
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /bin/false a2a && mkdir -p /data && chown a2a:a2a /data

COPY --from=builder /build/target/release/a2a-gateway /usr/local/bin/a2a-gateway

WORKDIR /app
USER a2a
EXPOSE 7240 7241 7242 7243

CMD ["a2a-gateway"]

# ── Runtime: orchestrator ─────────────────────────────────────────────────────

FROM debian:bookworm-slim AS orchestrator

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/orchestrator /usr/local/bin/orchestrator

EXPOSE 8080
# TCP liveness: checks the gRPC port is bound without needing grpc_health_probe
HEALTHCHECK --interval=15s --timeout=5s --retries=3 \
    CMD bash -c '</dev/tcp/localhost/8080' 2>/dev/null || exit 1
CMD ["orchestrator"]

# ── Runtime: research ─────────────────────────────────────────────────────────

FROM debian:bookworm-slim AS research

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/research /usr/local/bin/research

EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=5s --retries=3 \
    CMD bash -c '</dev/tcp/localhost/8080' 2>/dev/null || exit 1
CMD ["research"]

# ── Runtime: writer ───────────────────────────────────────────────────────────

FROM debian:bookworm-slim AS writer

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/writer /usr/local/bin/writer

EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=5s --retries=3 \
    CMD bash -c '</dev/tcp/localhost/8080' 2>/dev/null || exit 1
CMD ["writer"]

# ── Runtime: sidecar ──────────────────────────────────────────────────────────

FROM debian:bookworm-slim AS sidecar

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/a2a-sidecar /usr/local/bin/a2a-sidecar

# Socket directory — volume-mounted at runtime so Python/JS agents can reach it
RUN mkdir -p /run/a2a

CMD ["a2a-sidecar"]

// Re-export internal modules so integration tests in tests/ can import them.
// The public surface is intentionally wide for testing; this is not a published crate.

// Phase 1
pub mod config;
pub mod db;
pub mod identity;
pub mod local_server;
pub mod reconciler;
pub mod registry;
pub mod router;

// Phase 2 — Data layer
pub mod projects;
pub mod object_store;
pub mod file_sync;
pub mod task_log;
pub mod groups;
pub mod contributions;
pub mod onboarding;
pub mod context_builder;

// Phase 2 — Network layer
pub mod auth;
pub mod peer_client;
pub mod peer_server;
pub mod peer_sync;
pub mod nats_bus;
pub mod http_server;

// Phase 2 — Web UI
pub mod web;

// Phase 3 — LXC + Sidecar
pub mod lxc;

// Cross-cutting
pub mod error_class;
pub mod memory_service;

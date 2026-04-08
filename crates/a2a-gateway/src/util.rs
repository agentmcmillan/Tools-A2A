/// Shared utilities used across gateway modules.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current time as seconds since Unix epoch (f64).
/// Used for all timestamp columns in PostgreSQL (DOUBLE PRECISION).
pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

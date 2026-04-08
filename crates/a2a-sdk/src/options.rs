use std::time::Duration;

/// Options for a single outbound `call()` or `stream()`.
#[derive(Debug, Clone)]
pub struct CallOptions {
    /// Per-call timeout.  Default: 30 s.
    pub timeout: Duration,
    /// Number of retry attempts on transient failure (status Unavailable / DeadlineExceeded).
    /// Default: 3.
    pub retries: u32,
    /// Exponential-backoff multiplier.  Default: 2.0.
    pub backoff_factor: f64,
    /// Number of consecutive failures before the circuit opens.  Default: 5.
    pub circuit_breaker_threshold: u32,
}

impl Default for CallOptions {
    fn default() -> Self {
        Self {
            timeout:                    Duration::from_secs(30),
            retries:                    3,
            backoff_factor:             2.0,
            circuit_breaker_threshold:  5,
        }
    }
}

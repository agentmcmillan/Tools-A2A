/// Error classification for agent call responses.
///
/// Every error in the call path is classified into one of three categories,
/// which determines the gateway's automated response:
///
///   ┌────────────┐     auto-retry (3x, exp backoff)
///   │ Transient  │ ──► timeout, connection refused, 503, UNAVAILABLE
///   └────────────┘
///
///   ┌────────────┐     log as Failed, no retry
///   │ Permanent  │ ──► 400, 404, INVALID_ARGUMENT, NOT_FOUND, PERMISSION_DENIED
///   └────────────┘
///
///   ┌────────────┐     push notification to user, log as blocked
///   │ NeedsHuman │ ──► agent explicitly escalated, FAILED_PRECONDITION
///   └────────────┘

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Network issue, timeout, temporary unavailability — safe to retry.
    Transient,
    /// Agent rejected the request — do not retry; log and move on.
    Permanent,
    /// Agent explicitly requested human intervention.
    NeedsHuman,
}

impl ErrorClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Transient  => "transient",
            Self::Permanent  => "permanent",
            Self::NeedsHuman => "needs_human",
        }
    }

    /// Classify an HTTP status code.
    pub fn from_http_status(status: u16) -> Self {
        match status {
            // Transient: server errors and rate limits
            408 | 429 | 500 | 502 | 503 | 504 => Self::Transient,
            // Needs human: 409 Conflict often means manual resolution needed
            409 => Self::NeedsHuman,
            // Permanent: all other client errors
            400..=499 => Self::Permanent,
            // Anything else (1xx, 3xx, unknown) — treat as transient
            _ => Self::Transient,
        }
    }

    /// Classify a gRPC status code.
    pub fn from_grpc_code(code: tonic::Code) -> Self {
        use tonic::Code;
        match code {
            // Transient: retry-safe errors
            Code::Unavailable | Code::DeadlineExceeded | Code::Aborted
            | Code::ResourceExhausted | Code::Unknown | Code::Internal => Self::Transient,

            // Needs human: agent asked for human input
            Code::FailedPrecondition => Self::NeedsHuman,

            // Permanent: everything else
            Code::InvalidArgument | Code::NotFound | Code::AlreadyExists
            | Code::PermissionDenied | Code::Unauthenticated
            | Code::Unimplemented | Code::OutOfRange
            | Code::DataLoss | Code::Cancelled | Code::Ok => Self::Permanent,
        }
    }

    /// Classify a reqwest error (network-level).
    pub fn from_reqwest_error(e: &reqwest::Error) -> Self {
        if e.is_timeout() || e.is_connect() {
            Self::Transient
        } else if e.is_status() {
            e.status()
                .map(|s| Self::from_http_status(s.as_u16()))
                .unwrap_or(Self::Transient)
        } else {
            Self::Transient // decode errors, redirect loops, etc. — retry
        }
    }

    pub fn should_retry(&self) -> bool {
        matches!(self, Self::Transient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_classification() {
        assert_eq!(ErrorClass::from_http_status(503), ErrorClass::Transient);
        assert_eq!(ErrorClass::from_http_status(429), ErrorClass::Transient);
        assert_eq!(ErrorClass::from_http_status(404), ErrorClass::Permanent);
        assert_eq!(ErrorClass::from_http_status(400), ErrorClass::Permanent);
        assert_eq!(ErrorClass::from_http_status(409), ErrorClass::NeedsHuman);
    }

    #[test]
    fn grpc_classification() {
        assert_eq!(ErrorClass::from_grpc_code(tonic::Code::Unavailable), ErrorClass::Transient);
        assert_eq!(ErrorClass::from_grpc_code(tonic::Code::DeadlineExceeded), ErrorClass::Transient);
        assert_eq!(ErrorClass::from_grpc_code(tonic::Code::NotFound), ErrorClass::Permanent);
        assert_eq!(ErrorClass::from_grpc_code(tonic::Code::FailedPrecondition), ErrorClass::NeedsHuman);
    }

    #[test]
    fn retry_semantics() {
        assert!(ErrorClass::Transient.should_retry());
        assert!(!ErrorClass::Permanent.should_retry());
        assert!(!ErrorClass::NeedsHuman.should_retry());
    }
}

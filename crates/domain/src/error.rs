//! Structured error taxonomy.
//!
//! UI-facing messages are derived from the kind; diagnostic/source detail
//! never reaches the user directly.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{kind}: {detail}")]
pub struct ServiceError {
    pub kind: ErrorKind,
    /// Diagnostic context for logs. Never rendered to users verbatim.
    pub detail: String,
}

impl ServiceError {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Safe, user-renderable message. Deliberately coarse.
    pub fn ui_message(&self) -> &'static str {
        match self.kind {
            ErrorKind::NotConnected => "Not connected",
            ErrorKind::NotPaired => "This device is not linked yet",
            ErrorKind::InvalidRequest => "Invalid request",
            ErrorKind::Database => "Storage error",
            ErrorKind::StorageBusy => "Storage busy, try again shortly",
            ErrorKind::MediaUnavailable => "Media unavailable",
            ErrorKind::Timeout => "Operation timed out",
            ErrorKind::Cancelled => "Cancelled",
            ErrorKind::Protocol => "WhatsApp protocol error",
            ErrorKind::RateLimited => "Rate limited by WhatsApp",
            ErrorKind::Overloaded => "Busy, try again shortly",
            ErrorKind::Unsupported => "Not supported yet",
            ErrorKind::Internal => "Internal error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    NotConnected,
    NotPaired,
    InvalidRequest,
    Database,
    StorageBusy,
    MediaUnavailable,
    Timeout,
    Cancelled,
    Protocol,
    RateLimited,
    Overloaded,
    Unsupported,
    Internal,
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ErrorKind::NotConnected => "NotConnected",
            ErrorKind::NotPaired => "NotPaired",
            ErrorKind::InvalidRequest => "InvalidRequest",
            ErrorKind::Database => "Database",
            ErrorKind::StorageBusy => "StorageBusy",
            ErrorKind::MediaUnavailable => "MediaUnavailable",
            ErrorKind::Timeout => "Timeout",
            ErrorKind::Cancelled => "Cancelled",
            ErrorKind::Protocol => "Protocol",
            ErrorKind::RateLimited => "RateLimited",
            ErrorKind::Overloaded => "Overloaded",
            ErrorKind::Unsupported => "Unsupported",
            ErrorKind::Internal => "Internal",
        };
        f.write_str(s)
    }
}

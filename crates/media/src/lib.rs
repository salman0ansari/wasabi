//! Wasabi media subsystem.
//!
//! Design constants are non-negotiable resource budgets;
//! values are benchmark-tunable, never removable.

mod cache;
mod manager;
mod thumb;

pub use cache::DiskCache;
pub use manager::{MediaManager, media_downloadable};
pub use thumb::ThumbnailService;
pub use tokio_util::sync::CancellationToken;

// Re-exported so callers wiring downloads need no direct tokio-util/upstream
// dependency beyond this crate.
pub use whatsapp_rust::download::{DownloadParams, Downloadable};

/// Concurrent media downloads per account.
pub const MAX_CONCURRENT_DOWNLOADS: usize = 3;
/// Concurrent media uploads per account.
pub const MAX_CONCURRENT_UPLOADS: usize = 2;
/// Thumbnail/image-decode workers (CPU budget).
pub const MAX_DECODE_WORKERS: usize = 2;
/// Pending media operations before new requests are rejected `Overloaded`.
pub const MEDIA_QUEUE_CAPACITY: usize = 32;
/// Decoded-image/thumbnail LRU byte budget.
pub const DECODED_IMAGE_CACHE_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
/// Default disk cache quota (configurable at runtime).
pub const DEFAULT_DISK_CACHE_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("media unavailable")]
    Unavailable,
    #[error("media queue full")]
    Overloaded,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("invalid argument: {0}")]
    InvalidInput(String),
    /// Transport/CDN failure surfaced from the upstream client; the original
    /// error is flattened because co-waiters must be able to clone outcomes.
    #[error("download failed: {0}")]
    Download(String),
    #[error("decode failed: {0}")]
    Decode(String),
}

// Manual Clone: co-waiters on a shared download receive an outcome copy, and
// io::Error itself is not clonable — its kind plus message carry everything
// callers branch on.
impl Clone for MediaError {
    fn clone(&self) -> Self {
        match self {
            Self::Unavailable => Self::Unavailable,
            Self::Overloaded => Self::Overloaded,
            Self::Io(e) => Self::Io(std::io::Error::new(e.kind(), e.to_string())),
            Self::Cancelled => Self::Cancelled,
            Self::InvalidInput(s) => Self::InvalidInput(s.clone()),
            Self::Download(s) => Self::Download(s.clone()),
            Self::Decode(s) => Self::Decode(s.clone()),
        }
    }
}

// Same rationale as Clone: shared outcomes are asserted against in tests and
// compared by co-waiters, so Io flattens to kind + message.
impl PartialEq for MediaError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unavailable, Self::Unavailable)
            | (Self::Overloaded, Self::Overloaded)
            | (Self::Cancelled, Self::Cancelled) => true,
            (Self::Io(a), Self::Io(b)) => a.kind() == b.kind() && a.to_string() == b.to_string(),
            (Self::InvalidInput(a), Self::InvalidInput(b))
            | (Self::Download(a), Self::Download(b))
            | (Self::Decode(a), Self::Decode(b)) => a == b,
            _ => false,
        }
    }
}

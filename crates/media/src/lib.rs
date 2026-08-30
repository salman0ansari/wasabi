//! Wasabi media subsystem.
//!
//! Design constants are non-negotiable resource budgets;
//! values are benchmark-tunable, never removable.

mod cache;
mod manager;
mod thumb;

pub use cache::{DiskCache, avatar_cache_key, thumb_cache_key};
pub use manager::{ClientProvider, MediaManager, StagedUpload, media_downloadable};
pub use thumb::ThumbnailService;
pub use tokio_util::sync::CancellationToken;

// Re-exported so callers wiring downloads need no direct tokio-util/upstream
// dependency beyond this crate.
pub use whatsapp_rust::download::{DownloadParams, Downloadable};
pub use whatsapp_rust::upload::UploadResponse;

/// Concurrent media downloads per account.
pub const MAX_CONCURRENT_DOWNLOADS: usize = 3;
/// Concurrent media uploads per account.
pub const MAX_CONCURRENT_UPLOADS: usize = 2;
/// Conservative protocol/file-system guard. Product policy may lower this by
/// media class, but no composer operation may exceed two GiB.
pub const MAX_OUTGOING_ATTACHMENT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Thumbnail/image-decode workers (CPU budget).
pub const MAX_DECODE_WORKERS: usize = 2;
/// Pending media operations before new requests are rejected `Overloaded`.
pub const MEDIA_QUEUE_CAPACITY: usize = 32;
/// Decoded-image/thumbnail LRU byte budget.
pub const DECODED_IMAGE_CACHE_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
/// Default disk cache quota (configurable at runtime).
pub const DEFAULT_DISK_CACHE_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Preview profile photos are small; larger bodies are rejected as unavailable.
pub const MAX_PROFILE_PICTURE_BYTES: u64 = 512 * 1024;

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
    #[error("upload failed: {0}")]
    Upload(String),
    #[error("media encryption failed: {0}")]
    Encryption(String),
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
            Self::Upload(s) => Self::Upload(s.clone()),
            Self::Encryption(s) => Self::Encryption(s.clone()),
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
            | (Self::Upload(a), Self::Upload(b))
            | (Self::Encryption(a), Self::Encryption(b))
            | (Self::Decode(a), Self::Decode(b)) => a == b,
            _ => false,
        }
    }
}

//! Wasabi media subsystem (Phase 8 implements; budgets and types fixed now
//! so the rest of the codebase can depend on them).
//!
//! Design constants are non-negotiable resource budgets (charter §107);
//! values are benchmark-tunable, never removable.

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
}

use thiserror::Error;
use wacore::store::error::StoreError;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChatStoreError {
    #[error("storage error")]
    Store(#[from] StoreError),

    #[error("invalid full-text search query")]
    InvalidSearchQuery,

    /// A writer batch rolled back; the writes acknowledged by this `flush`
    /// were dropped. Carries the underlying error rendered to text (one batch
    /// outcome fans out to many flush waiters).
    #[error("write batch failed: {0}")]
    WriteBatchFailed(String),

    /// The bounded writer-ingress queue is full (writer stalled or burst too
    /// large). The write was NOT accepted; retry after backoff. Never a
    /// correctness hazard by itself — but hosts that cannot tolerate losing
    /// the enqueue must await the async variants instead of ignoring this.
    #[error("writer ingress full")]
    IngressFull,
}

pub type Result<T> = std::result::Result<T, ChatStoreError>;

pub(crate) fn db_err(e: diesel::result::Error) -> StoreError {
    StoreError::Database(Box::new(e))
}

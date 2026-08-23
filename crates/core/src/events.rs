//! Bounded UI-facing event surfaces.
//!
//! - Ephemeral state (connection state, QR, typing, presence, progress)
//!   travels through last-value-wins watch channels.
//! - Durable change signals are invalidations only: "something changed —
//!   re-query". Rows never ride through events.
//! - The invalidation channel is a bounded broadcast: a lagging consumer gets
//!   `Lagged` and MUST refresh its visible projections from durable state.
//!   Stale deltas are never replayed.

use tokio::sync::watch;

/// Connection/session state for one account; latest value wins.
pub type ConnectionStateWatch = watch::Receiver<crate::state::SessionState>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalidation {
    /// Chat list changed (ordering/previews/unread).
    Chats,
    /// The message set of one chat changed.
    Messages { chat: String },
    /// Contact naming changed.
    Contacts,
}

/// Capacity of the invalidation broadcast. Matches the upstream StoreChange
/// feed so both layers share one lag policy.
pub const INVALIDATION_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct InvalidationPublisher {
    tx: tokio::sync::broadcast::Sender<Invalidation>,
}

impl Default for InvalidationPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl InvalidationPublisher {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(INVALIDATION_CAPACITY);
        Self { tx }
    }

    /// Non-blocking publish; no consumer or full buffer simply means nobody
    /// needs this signal right now (they recover by re-querying).
    pub fn publish(&self, inv: Invalidation) {
        let _ = self.tx.send(inv);
    }

    pub fn subscribe(&self) -> InvalidationFeed {
        InvalidationFeed {
            rx: self.tx.subscribe(),
        }
    }
}

/// Consumer half. `Lagged` is a normal recovery path, not an error to
/// escalate: treat every projection as dirty and re-query once.
#[must_use]
pub struct InvalidationFeed {
    rx: tokio::sync::broadcast::Receiver<Invalidation>,
}

impl InvalidationFeed {
    pub async fn recv(&mut self) -> Option<Invalidation> {
        match self.rx.recv().await {
            Ok(inv) => Some(inv),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Some(Invalidation::Chats),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    }

    pub fn try_recv(&mut self) -> Option<Invalidation> {
        match self.rx.try_recv() {
            Ok(inv) => Some(inv),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                Some(Invalidation::Chats)
            }
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => None,
        }
    }

    /// Drain everything buffered into a coalesced set (order-preserving,
    /// deduplicated). A `Lagged` anywhere collapses into a coarse `Chats`
    /// invalidation because per-chat detail is no longer trustworthy.
    pub fn drain_coalesced(&mut self) -> Vec<Invalidation> {
        let mut out: Vec<Invalidation> = Vec::new();
        let mut lagged = false;
        loop {
            match self.rx.try_recv() {
                Ok(inv) => {
                    if !out.contains(&inv) {
                        out.push(inv);
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    lagged = true;
                }
                Err(
                    tokio::sync::broadcast::error::TryRecvError::Empty
                    | tokio::sync::broadcast::error::TryRecvError::Closed,
                ) => break,
            }
        }
        if lagged && !out.contains(&Invalidation::Chats) {
            out.insert(0, Invalidation::Chats);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn last_value_and_lag_recovery() {
        let pub_ = InvalidationPublisher::new();
        let mut feed = pub_.subscribe();
        // Drop the initial receiver slot by subscribing a throwaway first?
        // No: publisher keeps one alive internally via `new`'s dropped rx —
        // sends succeed regardless of receivers.
        drop(pub_.subscribe());

        assert_eq!(feed.try_recv(), None);

        // Overflow the buffer deliberately: 300 > 256 capacity.
        for i in 0..300u32 {
            pub_.publish(Invalidation::Messages {
                chat: format!("c{i}"),
            });
        }
        let drained = feed.drain_coalesced();
        assert!(
            drained.iter().any(|i| matches!(i, Invalidation::Chats)),
            "lag must collapse into coarse invalidation"
        );
    }
}

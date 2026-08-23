//! Session lifecycle plumbing: bot assembly, the pairing QR feed, and the
//! mapping from vendored-library events onto the domain state machine.
//!
//! The event pump is the only component here that runs concurrently, and its
//! ownership is explicit: it holds clones of the state/QR watch senders plus a
//! [`CancellationToken`], the session stores its join handle, and teardown
//! cancels first and joins second.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use wasabi_core::state::SessionState;
use wasabi_repository::AccountStore;
use whatsapp_rust::types::events::Event;
use whatsapp_rust_sqlite_storage::SqliteStore;

/// Latest pairing QR issued by the server. The payload embeds device identity
/// material, so it is carried opaquely and never logged anywhere downstream.
#[derive(Clone)]
pub struct QrState {
    pub code: String,
    /// Validity window the library quoted for this specific code; codes rotate
    /// well before pairing gives up.
    pub expires_in: Duration,
}

impl std::fmt::Debug for QrState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omits `code`: a stray Debug of the QR must not leak the
        // pairing secret into logs.
        f.debug_struct("QrState")
            .field("code_len", &self.code.len())
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("session is already running")]
    AlreadyRunning,
    #[error("bot assembly failed")]
    Build(#[from] whatsapp_rust::bot::BotBuilderError),
}

/// Capacity of the pump's own mailbox between the library's ordered event
/// drainer and the state machine.
pub(crate) const PUMP_MAILBOX_CAPACITY: usize = 1024;

/// How long teardown waits for the pump to observe cancellation before giving
/// up on joining it. Cancellation is checked on every receive, so the normal
/// exit is immediate; the cap only bounds a pathological wedge.
pub(crate) const PUMP_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct Pump {
    pub token: CancellationToken,
    pub join: tokio::task::JoinHandle<()>,
}

/// Single seam to repository internals: the Bot needs the protocol store as
/// its storage Backend, and every other accessor on `AccountStore` is
/// facade-level. Requires `AccountStore::sqlite(&self) -> &Arc<SqliteStore>`
/// on the repository side.
pub(crate) fn protocol_backend(store: &AccountStore) -> Arc<SqliteStore> {
    Arc::clone(store.sqlite())
}

/// Spawn the event pump: drains the mailbox and folds each event into the
/// session state and QR watches. Exits on token cancellation or once every
/// sender (the client's registered handler) is gone.
pub(crate) fn spawn_event_pump(
    events: whatsapp_rust::async_channel::Receiver<Arc<Event>>,
    state_tx: watch::Sender<SessionState>,
    qr_tx: watch::Sender<Option<QrState>>,
    token: CancellationToken,
) -> Pump {
    let join = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                () = token.cancelled() => break,
                received = events.recv() => match received {
                    Ok(event) => event,
                    // All senders dropped: the client (and with it the whole
                    // run loop) is gone.
                    Err(_) => break,
                },
            };
            apply_event(&event, &state_tx, &qr_tx);
        }
    });
    Pump { token, join }
}

/// Publish a transition, logging (never panicking on) table violations: late
/// or duplicated signals from the library are expected noise.
pub(crate) fn transition_to(state_tx: &watch::Sender<SessionState>, next: SessionState) {
    let current = state_tx.borrow().clone();
    match current.transition(next.clone()) {
        Ok(next) => {
            let _ = state_tx.send(next);
        }
        Err(e) => warn!(from = %e.from, to = %e.to, "rejected invalid session transition"),
    }
}

/// Like [`transition_to`], but accepts any of `candidates` in order — for
/// signals whose right label depends on where the session currently is.
fn transition_to_any(
    state_tx: &watch::Sender<SessionState>,
    candidates: &[SessionState],
    signal: &'static str,
) {
    let current = state_tx.borrow().clone();
    for candidate in candidates {
        if current.transition(candidate.clone()).is_ok() {
            let _ = state_tx.send(candidate.clone());
            return;
        }
    }
    warn!(signal, from = %current, "no valid session transition for event");
}

fn apply_event(
    event: &Event,
    state_tx: &watch::Sender<SessionState>,
    qr_tx: &watch::Sender<Option<QrState>>,
) {
    match event {
        Event::PairingQrCode(qr) => {
            transition_to(state_tx, SessionState::Pairing);
            // A spent code must vanish the moment the next one rotates in;
            // consumers render whichever payload is current.
            let _ = qr_tx.send(Some(QrState {
                code: qr.code.clone(),
                expires_in: qr.timeout,
            }));
        }
        Event::PairingQrCodesExhausted(_) => {
            // Rotation gave up; the last displayed code is dead.
            let _ = qr_tx.send(None);
        }
        Event::PairSuccess(_) => {
            let _ = qr_tx.send(None);
            transition_to(state_tx, SessionState::Connecting);
        }
        Event::Connected(_) => {
            let _ = qr_tx.send(None);
            // Via Connecting because the table has no Pairing/Failed →
            // Connected edge; relink-after-reconnect lands here too.
            transition_to(state_tx, SessionState::Connecting);
            transition_to(state_tx, SessionState::Connected);
        }
        Event::LoggedOut(logout) => {
            let _ = qr_tx.send(None);
            let reason = if logout.reason.is_logged_out() {
                "logged out"
            } else {
                "forced logout"
            };
            transition_to(state_tx, SessionState::Failed {
                reason: reason.to_string(),
            });
        }
        // A rejected pair attempt leaves rotation running; the next
        // PairingQrCode refreshes the watch.
        Event::PairError(_) => {}
        Event::Disconnected(disconnected) => {
            debug!(reason = %disconnected.reason, "transport down; library reconnect loop owns recovery");
            transition_to_any(
                state_tx,
                &[
                    SessionState::Reconnecting,
                    SessionState::Disconnected {
                        reason: Some(disconnected.reason.to_string()),
                    },
                ],
                "disconnected",
            );
        }
        Event::StreamError(error) => {
            debug!(code = %error.code, "stream error; library retries");
            transition_to_any(
                state_tx,
                &[
                    SessionState::Reconnecting,
                    SessionState::Disconnected {
                        reason: Some("stream error".to_string()),
                    },
                ],
                "stream error",
            );
        }
        Event::ConnectFailure(failure) => {
            transition_to(state_tx, SessionState::Failed {
                reason: format!("connect failure: {:?}", failure.reason),
            });
        }
        Event::ClientOutdated(_) => {
            transition_to(state_tx, SessionState::Failed {
                reason: "client outdated".to_string(),
            });
        }
        Event::StreamReplaced(_) => {
            transition_to(state_tx, SessionState::Failed {
                reason: "stream replaced by another session".to_string(),
            });
        }
        _ => {}
    }
}

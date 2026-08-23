//! Connection-state mirror plus pairing countdown anchor.

use std::time::Instant;

use wasabi_core::state::SessionState;

/// Last-value-wins projection of the session watches. The QR payload itself
/// is secret material and never stored here; only its validity window is.
#[derive(Clone)]
pub struct SessionMirror {
    pub state: SessionState,
    /// Wall-clock instant at which the current QR code expires.
    pub qr_deadline: Option<Instant>,
}

impl SessionMirror {
    pub fn new() -> Self {
        Self {
            state: SessionState::Stopped,
            qr_deadline: None,
        }
    }

    /// Composer gate: sending is allowed while the library owns a live
    /// transport (it buffers during brief reconnects).
    pub fn can_send(&self) -> bool {
        self.state.can_send()
    }

    pub fn status_label(&self) -> &'static str {
        match &self.state {
            SessionState::Stopped => "Offline",
            SessionState::Pairing => "Pairing",
            SessionState::Connecting => "Connecting",
            SessionState::Connected => "Connected",
            SessionState::Reconnecting => "Reconnecting",
            SessionState::Disconnected { .. } => "Disconnected",
            SessionState::LoggingOut => "Logging out",
            SessionState::Failed { .. } => "Error",
        }
    }

    /// True when the account has no live session and the UI should offer
    /// linking instead of a conversation.
    pub fn needs_pairing(&self) -> bool {
        matches!(self.state, SessionState::Stopped)
    }
}

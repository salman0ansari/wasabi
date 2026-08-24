//! Connection-state mirror plus pairing status and countdown anchor.

use std::time::Instant;

use wasabi_core::state::SessionState;

/// Last-value-wins projection of the session watches plus transient pairing
/// feedback owned by the desktop surface.
#[derive(Clone)]
pub struct SessionMirror {
    pub state: SessionState,
    /// Becomes true after the account has completed one live connection.
    /// Startup stays on the pairing surface until this is known.
    pub connected_once: bool,
    /// Ephemeral QR payload used only to render the current pairing code.
    /// It is not logged or persisted and is cleared when pairing ends.
    pub qr_code: Option<String>,
    /// Wall-clock instant at which the current QR code expires.
    pub qr_deadline: Option<Instant>,
    /// Whether a user-triggered pairing request is still being started.
    pub pairing_requesting: bool,
    /// Last user-visible error from a pairing request.
    pub pairing_error: Option<String>,
    /// Whether the alternative phone-number code surface is active.
    pub use_phone_pairing: bool,
    /// Ephemeral eight-character link code; never logged or persisted.
    pub phone_pair_code: Option<String>,
    /// Wall-clock instant at which the phone link code expires.
    pub phone_pair_deadline: Option<Instant>,
    /// Whether a phone-code request is currently in flight.
    pub phone_pair_requesting: bool,
    /// Last user-visible phone-code request error.
    pub phone_pair_error: Option<String>,
}

impl SessionMirror {
    pub fn new() -> Self {
        Self {
            state: SessionState::Stopped,
            connected_once: false,
            qr_code: None,
            qr_deadline: None,
            pairing_requesting: false,
            pairing_error: None,
            use_phone_pairing: false,
            phone_pair_code: None,
            phone_pair_deadline: None,
            phone_pair_requesting: false,
            phone_pair_error: None,
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

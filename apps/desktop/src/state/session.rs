//! Connection-state mirror plus pairing status and countdown anchor.

use std::time::Instant;

use wasabi_core::state::{SessionState, failure_reason};

/// What the user can actually do from a recovery surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    None,
    /// Device is unlinked; start pairing. Cached chats stay on disk.
    LinkDevice,
    /// A retry can be issued (reconnect or request pairing again).
    Retry,
}

/// User-facing copy for a session state. Failed reasons are matched as
/// stable labels; unrecognized strings are never dumped into the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryCopy {
    pub status_label: &'static str,
    pub headline: String,
    pub detail: String,
    pub action: RecoveryAction,
    pub action_label: Option<&'static str>,
}

impl RecoveryCopy {
    pub fn banner_text(&self) -> String {
        if self.detail.is_empty() {
            self.headline.clone()
        } else {
            format!("{} — {}", self.headline, self.detail)
        }
    }
}

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
        recovery_copy(&self.state).status_label
    }

    pub fn recovery_copy(&self) -> RecoveryCopy {
        recovery_copy(&self.state)
    }

    /// True when the account has no live session and the UI should offer
    /// linking instead of a conversation.
    pub fn needs_pairing(&self) -> bool {
        matches!(self.state, SessionState::Stopped)
    }
}

pub fn recovery_copy(state: &SessionState) -> RecoveryCopy {
    match state {
        SessionState::Stopped => RecoveryCopy {
            status_label: "Offline",
            headline: "Offline".into(),
            detail: "Cached messages remain available.".into(),
            action: RecoveryAction::None,
            action_label: None,
        },
        SessionState::Pairing => RecoveryCopy {
            status_label: "Pairing",
            headline: "Pairing".into(),
            detail: String::new(),
            action: RecoveryAction::None,
            action_label: None,
        },
        SessionState::Connecting => RecoveryCopy {
            status_label: "Connecting",
            headline: "Connecting".into(),
            detail: "Cached messages remain available.".into(),
            action: RecoveryAction::None,
            action_label: None,
        },
        SessionState::Connected => RecoveryCopy {
            status_label: "Connected",
            headline: "Connected".into(),
            detail: String::new(),
            action: RecoveryAction::None,
            action_label: None,
        },
        SessionState::Reconnecting => RecoveryCopy {
            status_label: "Reconnecting",
            headline: "Reconnecting".into(),
            detail: "Cached messages remain available.".into(),
            action: RecoveryAction::None,
            action_label: None,
        },
        SessionState::Disconnected { .. } => RecoveryCopy {
            status_label: "Disconnected",
            headline: "Disconnected".into(),
            detail: "Cached messages remain available.".into(),
            action: RecoveryAction::Retry,
            action_label: Some("Try again"),
        },
        SessionState::LoggingOut => RecoveryCopy {
            status_label: "Logging out",
            headline: "Logging out".into(),
            detail: String::new(),
            action: RecoveryAction::None,
            action_label: None,
        },
        SessionState::Failed { reason } => failed_recovery(reason),
    }
}

fn failed_recovery(reason: &str) -> RecoveryCopy {
    if reason == failure_reason::FORCED_LOGOUT {
        RecoveryCopy {
            status_label: "Unlinked",
            headline: "This device was unlinked from the phone".into(),
            detail: "Cached chats remain on this computer.".into(),
            action: RecoveryAction::LinkDevice,
            action_label: Some("Link this device"),
        }
    } else if reason == failure_reason::LOGGED_OUT {
        RecoveryCopy {
            status_label: "Logged out",
            headline: "This device is no longer linked".into(),
            detail: "Cached chats remain on this computer.".into(),
            action: RecoveryAction::LinkDevice,
            action_label: Some("Link this device"),
        }
    } else if reason == failure_reason::CLIENT_OUTDATED {
        RecoveryCopy {
            status_label: "Update required",
            headline: "This version of wasabi is too old for WhatsApp".into(),
            detail: "An updated wasabi is required. Retrying will not help.".into(),
            action: RecoveryAction::None,
            action_label: None,
        }
    } else if reason == failure_reason::RATE_LIMITED {
        RecoveryCopy {
            status_label: "Rate limited",
            headline: "WhatsApp is rate-limiting this device.".into(),
            detail: "Wait, then try again.".into(),
            action: RecoveryAction::Retry,
            action_label: Some("Try again"),
        }
    } else if failure_reason::is_temporarily_banned(reason) {
        let detail = match failure_reason::temporary_ban_wait_secs(reason) {
            Some(secs) => format!(
                "You can use WhatsApp again in {}. Cached chats remain available.",
                format_wait(secs)
            ),
            None => {
                "This account or device is temporarily restricted. Cached chats remain available."
                    .into()
            }
        };
        RecoveryCopy {
            status_label: "Restricted",
            headline: "This account or device is temporarily restricted".into(),
            detail,
            action: RecoveryAction::None,
            action_label: None,
        }
    } else {
        RecoveryCopy {
            status_label: "Error",
            headline: "Connection needs attention".into(),
            detail: "Cached messages remain available.".into(),
            action: RecoveryAction::Retry,
            action_label: Some("Try again"),
        }
    }
}

fn format_wait(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    let mut parts = Vec::new();
    push_unit(&mut parts, days, "day", "days");
    push_unit(&mut parts, hours, "hour", "hours");
    push_unit(&mut parts, minutes, "minute", "minutes");
    if parts.is_empty() {
        push_unit(&mut parts, secs.max(1), "second", "seconds");
    }
    parts.join(" ")
}

fn push_unit(parts: &mut Vec<String>, value: i64, singular: &str, plural: &str) {
    if value <= 0 {
        return;
    }
    if value == 1 {
        parts.push(format!("1 {singular}"));
    } else {
        parts.push(format!("{value} {plural}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasabi_domain::RATE_LIMITED_DEVICE;

    fn failed(reason: &str) -> RecoveryCopy {
        recovery_copy(&SessionState::Failed {
            reason: reason.to_string(),
        })
    }

    fn assert_no_protocol_leak(copy: &RecoveryCopy) {
        let blob = format!("{copy:?}{}{}", copy.headline, copy.detail);
        assert!(!blob.contains("Unknown("), "{blob}");
        assert!(!blob.contains("ConnectFailure"));
        assert!(!blob.contains("TempBanReason"));
        assert!(!blob.contains('@'));
        assert!(!blob.contains("s.whatsapp.net"));
        assert!(!blob.contains("1555"));
    }

    #[test]
    fn each_failure_reason_has_distinct_copy_and_recovery() {
        let forced = failed(failure_reason::FORCED_LOGOUT);
        let logged = failed(failure_reason::LOGGED_OUT);
        let outdated = failed(failure_reason::CLIENT_OUTDATED);
        let limited = failed(failure_reason::RATE_LIMITED);
        let banned = failed("temporarily banned: 3600");
        let banned_no_wait = failed(failure_reason::TEMPORARILY_BANNED);
        let generic = failed("connect failure: Unknown(418)");

        assert_eq!(forced.status_label, "Unlinked");
        assert_eq!(forced.action, RecoveryAction::LinkDevice);
        assert_eq!(forced.action_label, Some("Link this device"));
        assert!(forced.headline.contains("unlinked from the phone"));
        assert!(forced.detail.to_lowercase().contains("cached"));

        assert_eq!(logged.status_label, "Logged out");
        assert_eq!(logged.action, RecoveryAction::LinkDevice);
        assert!(!logged.headline.to_lowercase().contains("forced"));
        assert!(!logged.detail.to_lowercase().contains("forced"));
        assert!(!logged.headline.contains("unlinked from the phone"));
        assert_ne!(logged.headline, forced.headline);

        assert_eq!(outdated.status_label, "Update required");
        assert_eq!(outdated.action, RecoveryAction::None);
        assert!(outdated.headline.contains("too old"));
        assert!(outdated.detail.contains("updated wasabi"));
        assert!(outdated.detail.contains("Retrying will not help"));

        assert_eq!(limited.status_label, "Rate limited");
        assert_eq!(limited.action, RecoveryAction::Retry);
        assert_eq!(limited.action_label, Some("Try again"));
        assert!(limited.headline.contains("rate-limiting this device"));
        assert_eq!(
            format!("{} {}", limited.headline, limited.detail).trim(),
            RATE_LIMITED_DEVICE
        );

        assert_eq!(banned.status_label, "Restricted");
        assert_eq!(banned.action, RecoveryAction::None);
        assert!(banned.detail.contains("1 hour"));
        assert!(!banned.detail.contains("0 second"));
        assert_eq!(banned_no_wait.action, RecoveryAction::None);
        assert!(!banned_no_wait.detail.contains("0 second"));
        assert!(!banned_no_wait.detail.contains("again in"));

        assert_eq!(generic.status_label, "Error");
        assert_eq!(generic.action, RecoveryAction::Retry);
        assert!(!generic.headline.contains("Unknown"));
        assert!(!generic.detail.contains("Unknown(418)"));
        assert!(!generic.banner_text().contains("Unknown"));

        let copies = [&forced, &logged, &outdated, &limited, &banned, &generic];
        for (index, copy) in copies.iter().enumerate() {
            assert_no_protocol_leak(copy);
            for other in copies.iter().skip(index + 1) {
                assert_ne!(
                    (copy.status_label, copy.headline.as_str()),
                    (other.status_label, other.headline.as_str()),
                    "copy collided"
                );
            }
        }
    }

    #[test]
    fn status_label_is_honest_for_settings() {
        let mut session = SessionMirror::new();
        session.state = SessionState::Failed {
            reason: failure_reason::CLIENT_OUTDATED.to_string(),
        };
        assert_eq!(session.status_label(), "Update required");
        session.state = SessionState::Failed {
            reason: failure_reason::FORCED_LOGOUT.to_string(),
        };
        assert_eq!(session.status_label(), "Unlinked");
        session.state = SessionState::Connected;
        assert_eq!(session.status_label(), "Connected");
    }
}

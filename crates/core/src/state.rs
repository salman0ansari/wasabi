//! Session lifecycle state machine.
//!
//! Serialized transitions; impossible-state booleans do not exist.

use std::fmt;

/// Stable [`SessionState::Failed`] labels. The UI matches these strings
/// (or the `temporarily banned` prefix); do not put `Debug` dumps here.
pub mod failure_reason {
    pub const LOGGED_OUT: &str = "logged out";
    pub const FORCED_LOGOUT: &str = "forced logout";
    pub const CLIENT_OUTDATED: &str = "client outdated";
    pub const RATE_LIMITED: &str = "rate limited";
    pub const TEMPORARILY_BANNED: &str = "temporarily banned";
    pub const CONNECT_FAILURE: &str = "connect failure";
    pub const STREAM_REPLACED: &str = "stream replaced by another session";

    pub fn temporarily_banned(wait_secs: i64) -> String {
        if wait_secs > 0 {
            format!("{TEMPORARILY_BANNED}: {wait_secs}")
        } else {
            TEMPORARILY_BANNED.to_string()
        }
    }

    pub fn is_temporarily_banned(reason: &str) -> bool {
        reason == TEMPORARILY_BANNED
            || reason
                .strip_prefix(TEMPORARILY_BANNED)
                .is_some_and(|rest| rest.starts_with(':'))
    }

    pub fn temporary_ban_wait_secs(reason: &str) -> Option<i64> {
        reason
            .strip_prefix(TEMPORARILY_BANNED)?
            .strip_prefix(": ")?
            .parse()
            .ok()
            .filter(|secs| *secs > 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// No session; nothing running for the account.
    Stopped,
    /// QR/pair-code pairing in flight (single-flight per account).
    Pairing,
    Connecting,
    Connected,
    /// Transport down; library-owned reconnect loop active.
    Reconnecting,
    Disconnected {
        reason: Option<String>,
    },
    LoggingOut,
    Failed {
        reason: String,
    },
}

impl SessionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, SessionState::Connected)
    }

    pub fn can_send(&self) -> bool {
        matches!(self, SessionState::Connected | SessionState::Reconnecting)
    }

    /// Legal transition table. Anything else is a programming error and is
    /// rejected loudly rather than silently coerced.
    pub fn transition(&self, next: SessionState) -> Result<SessionState, InvalidTransition> {
        use SessionState::*;
        let ok = match (self, &next) {
            // Startup paths
            (Stopped, Pairing | Connecting) => true,
            (Pairing, Connecting | Stopped | Failed { .. }) => true,
            // Connection lifecycle
            (
                Connecting,
                Connected | Reconnecting | Disconnected { .. } | Failed { .. } | Stopped,
            ) => true,
            // Companion-link flow: the socket connects first, then the server
            // pushes pairing refs — Pairing is a legitimate post-connect state.
            (Connecting, Pairing) => true,
            (Connected, Reconnecting | Disconnected { .. } | LoggingOut | Failed { .. }) => true,
            (Reconnecting, Connected | Disconnected { .. } | LoggingOut | Failed { .. }) => true,
            (Disconnected { .. }, Connecting | Pairing | Stopped) => true,
            // Teardown paths
            (LoggingOut, Stopped | Failed { .. }) => true,
            (Failed { .. }, Stopped | Pairing | Connecting) => true,
            // Idempotence: announcing the same state again is fine.
            _ if self == &next => true,
            _ => false,
        };
        if ok {
            Ok(next)
        } else {
            Err(InvalidTransition {
                from: self.clone(),
                to: next,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: SessionState,
    pub to: SessionState,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionState::Stopped => write!(f, "stopped"),
            SessionState::Pairing => write!(f, "pairing"),
            SessionState::Connecting => write!(f, "connecting"),
            SessionState::Connected => write!(f, "connected"),
            SessionState::Reconnecting => write!(f, "reconnecting"),
            SessionState::Disconnected { reason } => match reason {
                Some(r) => write!(f, "disconnected: {r}"),
                None => write!(f, "disconnected"),
            },
            SessionState::LoggingOut => write!(f, "logging out"),
            SessionState::Failed { reason } => write!(f, "failed: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        let mut s = SessionState::Stopped;
        s = s.transition(SessionState::Pairing).unwrap();
        s = s.transition(SessionState::Connecting).unwrap();
        s = s.transition(SessionState::Connected).unwrap();
        s = s.transition(SessionState::Reconnecting).unwrap();
        s = s.transition(SessionState::Connected).unwrap();
        s = s.transition(SessionState::LoggingOut).unwrap();
        assert_eq!(
            s.transition(SessionState::Stopped).unwrap(),
            SessionState::Stopped
        );
    }

    #[test]
    fn rejects_impossible() {
        let connected = SessionState::Connected;
        // Connected → Pairing makes no sense without teardown.
        assert!(connected.transition(SessionState::Pairing).is_err());
        // Stopped cannot become Connected directly.
        assert!(
            SessionState::Stopped
                .transition(SessionState::Connected)
                .is_err()
        );
    }

    #[test]
    fn idempotent() {
        let c = SessionState::Connected;
        assert_eq!(c.clone().transition(c).unwrap(), SessionState::Connected);
    }

    #[test]
    fn failed_can_start_pairing() {
        let failed = SessionState::Failed {
            reason: failure_reason::FORCED_LOGOUT.to_string(),
        };
        assert_eq!(
            failed.transition(SessionState::Pairing).unwrap(),
            SessionState::Pairing
        );
    }

    #[test]
    fn temporary_ban_label_omits_zero_wait() {
        assert_eq!(
            failure_reason::temporarily_banned(0),
            failure_reason::TEMPORARILY_BANNED
        );
        assert_eq!(
            failure_reason::temporarily_banned(90),
            "temporarily banned: 90"
        );
        assert_eq!(
            failure_reason::temporary_ban_wait_secs("temporarily banned: 90"),
            Some(90)
        );
        assert_eq!(
            failure_reason::temporary_ban_wait_secs(failure_reason::TEMPORARILY_BANNED),
            None
        );
    }
}

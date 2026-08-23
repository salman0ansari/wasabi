//! Session lifecycle state machine.
//!
//! Serialized transitions; impossible-state booleans do not exist.

use std::fmt;

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
}

//! Stable identity types.
//!
//! Message identity is the sender-chosen WhatsApp id, unique only within
//! `(chat, sender)` — never treated as globally unique . Chat ids are opaque strings here; PN/LID alias
//! resolution stays behind the repository/whatsapp boundary.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Sender-chosen WhatsApp message id (upstream `generate_message_id` shape:
/// 32 hex chars with a 3-char prefix). Opaque to the domain.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MessageId(String);

impl MessageId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact: ids are not secret, but keep logs terse and uniform.
        write!(f, "MessageId({})", self.0)
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque identity for one media payload inside a conversation.
///
/// The current repository derives this from the durable message identity. The
/// UI must treat it as an uninterpreted handle and always use it together with
/// the row's chat identity; CDN paths, media keys, and bytes never cross the
/// product boundary.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MediaId(String);

impl MediaId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MediaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaId(<opaque>)")
    }
}

/// A conversation key as seen by the domain. The repository normalizes
/// PN↔LID before persisting; the domain sees one canonical thread id.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChatId(String);

impl ChatId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ChatId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted shape: never print full jids by default.
        let mut hasher = DefaultHasher::default();
        self.0.hash(&mut hasher);
        write!(f, "ChatId(pn#{:016x})", hasher.finish())
    }
}

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Local account identifier (wasabi-level; maps to a per-account database).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountId(u64);

impl AccountId {
    pub const FIRST: AccountId = AccountId(1);

    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccountId({})", self.0)
    }
}

/// Keyset cursor tiebreak: arrival order inside the same millisecond.
/// Session-local by contract — never persisted across restarts. SQLite may
/// renumber rowids when VACUUM compacts the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalCursor(pub i64);

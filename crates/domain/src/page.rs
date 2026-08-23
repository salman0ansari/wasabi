//! Keyset pagination types (charter §38). OFFSET is banned.

use crate::ids::ChatId;
use crate::message::ChatSummary;

/// Cursor for the chat list: mirrors the upstream two-pass
/// `(pinned_at, last_message_ts, jid)` ordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatPageCursor {
    pub pinned_at_ms: Option<i64>,
    pub last_activity_ms: i64,
    pub chat: ChatId,
}

/// A keyset page of chat summaries.
#[derive(Clone, Debug)]
pub struct ChatPage {
    pub rows: Vec<ChatSummary>,
    pub next_after: Option<ChatPageCursor>,
}

//! Message/chat projection models — bounded, UI-shaped views over durable
//! state. The database is authoritative; these are read projections
//!.

use serde::{Deserialize, Serialize};

use crate::ChatKind;
use crate::ids::{ChatId, LocalCursor, MessageId};

/// One row of the virtualized chat list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatSummary {
    pub id: ChatId,
    pub kind: ChatKind,
    pub display_name: Option<String>,
    /// Milliseconds since epoch; ordering key for the chat list.
    pub last_activity_ms: i64,
    pub last_message_preview: Option<String>,
    /// -1 means "manually marked unread" sentinel (upstream semantics).
    pub unread_count: i64,
    pub pinned_at_ms: Option<i64>,
    pub muted_until_ms: Option<i64>,
    pub archived: bool,
    /// Wasabi device-local preference; distinct from protocol-backed pinning.
    pub favorite: bool,
    /// Non-empty draft body preview, hydrated with the chat page.
    pub draft_preview: Option<String>,
}

/// Direction of a message relative to this account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageDirection {
    Incoming,
    Outgoing,
}

/// Delivery lifecycle for outgoing messages (monotonic; upstream status scale).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MessageStatus {
    Pending,
    ServerAck,
    Delivered,
    Read,
    Failed,
}

/// Sender identity as displayed. Full JIDs never leave the repository layer
/// unredacted into logs; the UI receives display-ready pieces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderJid {
    /// Bare user identity (PN or LID canonical form).
    pub bare: String,
    /// Resolved display name if known.
    pub push_name: Option<String>,
}

/// One message row in the sliding window projection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: MessageId,
    pub chat: ChatId,
    pub direction: MessageDirection,
    pub sender: SenderJid,
    /// Sort timestamp (server-corrected when known), milliseconds.
    pub timestamp_ms: i64,
    /// Arrival-order tiebreak within the same millisecond (session-local).
    pub seq: LocalCursor,
    pub kind: MessageKind,
    pub status: MessageStatus,
    pub edited_at_ms: Option<i64>,
    pub revoked: bool,
    pub starred: bool,
}

/// Content kind actually rendered today. Media payloads stay behind media
/// handles — bytes never ride through here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    Text {
        body: String,
    },
    Image {
        caption: Option<String>,
        mime: Option<String>,
        media_key: Option<String>,
    },
    Video {
        caption: Option<String>,
        mime: Option<String>,
        media_key: Option<String>,
    },
    Audio {
        mime: Option<String>,
        media_key: Option<String>,
    },
    Document {
        file_name: Option<String>,
        mime: Option<String>,
        media_key: Option<String>,
    },
    Sticker {
        mime: Option<String>,
        media_key: Option<String>,
    },
    Reaction {
        emoji: String,
    },
    System {
        text: String,
    },
    Unknown,
}

/// A keyset page of messages, ordered newest→oldest.
#[derive(Clone, Debug)]
pub struct MessagePage {
    pub rows: Vec<MessageRow>,
    /// Cursor of the oldest row; pass back to fetch the page before it.
    /// `None` when the beginning of history is reached.
    pub next_before: Option<PageCursor>,
}

/// Opaque pagination cursor handed back by the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageCursor {
    pub timestamp_ms: i64,
    pub seq: LocalCursor,
}

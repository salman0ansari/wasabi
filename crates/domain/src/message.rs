//! Message/chat projection models — bounded, UI-shaped views over durable
//! state. The database is authoritative; these are read projections
//!.

use serde::{Deserialize, Serialize};

use crate::ids::{ChatId, LocalCursor, MediaId, MessageId};
use crate::{ChatKind, Draft};

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
    /// Full device-local composer state for restoring reply/edit context.
    #[serde(default)]
    pub draft: Option<Draft>,
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
    #[serde(default)]
    pub quoted: Option<QuotedMessage>,
    pub status: MessageStatus,
    pub edited_at_ms: Option<i64>,
    pub revoked: bool,
    pub starred: bool,
}

/// Conservative WhatsApp Web edit entry-point window. The server remains
/// authoritative and may reject an edit sooner for account-specific policy.
pub const MESSAGE_EDIT_WINDOW_MS: i64 = 20 * 60 * 1_000;

impl MessageRow {
    /// Whether the desktop product may offer text editing at `now_ms`.
    /// Pending/failed rows use Retry instead; incoming, revoked, media, and
    /// expired rows never expose an inert Edit command.
    pub fn can_edit_text_at(&self, now_ms: i64) -> bool {
        self.direction == MessageDirection::Outgoing
            && matches!(self.kind, MessageKind::Text { .. })
            && !self.revoked
            && matches!(
                self.status,
                MessageStatus::ServerAck | MessageStatus::Delivered | MessageStatus::Read
            )
            && now_ms >= self.timestamp_ms
            && now_ms.saturating_sub(self.timestamp_ms) <= MESSAGE_EDIT_WINDOW_MS
    }
}

/// Display-safe reply context projected from a message's protocol context.
/// The quoted payload is reduced to a short preview; nested protobuf content
/// and transport metadata never cross the product boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotedMessage {
    pub id: MessageId,
    pub sender: Option<String>,
    pub preview: String,
}

/// Whether a media payload can be recovered without exposing its transport
/// credentials to the UI. `Local` is reserved for a verified cache hit;
/// repository projections currently report `Remote` or `Unavailable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaAvailability {
    Remote,
    Local,
    Unavailable,
}

/// Display-safe metadata for a media payload. Download paths, encryption keys,
/// hashes, thumbnails, and raw bytes remain owned by the media service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDescriptor {
    pub id: MediaId,
    pub mime_type: Option<String>,
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
    pub duration_seconds: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub availability: MediaAvailability,
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
        media: MediaDescriptor,
    },
    Video {
        caption: Option<String>,
        video_note: bool,
        media: MediaDescriptor,
    },
    Audio {
        voice_note: bool,
        media: MediaDescriptor,
    },
    Document {
        media: MediaDescriptor,
    },
    Sticker {
        animated: bool,
        media: MediaDescriptor,
    },
    Reaction {
        emoji: String,
    },
    System {
        text: String,
    },
    Unavailable {
        reason: UnavailableMessageReason,
    },
    Unknown,
}

/// Why a durable message row cannot expose its original content on this
/// companion device. Each reason has a distinct recovery expectation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnavailableMessageReason {
    /// Encryption material may still arrive or a placeholder resend may heal
    /// the row; the content is not permanently lost yet.
    WaitingForDecryption,
    /// Companion devices are intentionally not given view-once payloads.
    ViewOnceOnPhone,
    /// Hosted fanout content is unavailable to companion clients.
    HostedContent,
    /// A known bot fanout cannot be rendered by this client.
    BotContent,
}

#[cfg(test)]
mod edit_tests {
    use super::*;

    fn row(timestamp_ms: i64) -> MessageRow {
        MessageRow {
            id: MessageId::new("message-a"),
            chat: ChatId::new("chat-a@s.whatsapp.net"),
            direction: MessageDirection::Outgoing,
            sender: SenderJid {
                bare: "me@s.whatsapp.net".to_string(),
                push_name: None,
            },
            timestamp_ms,
            seq: LocalCursor(1),
            kind: MessageKind::Text {
                body: "before".to_string(),
            },
            quoted: None,
            status: MessageStatus::Delivered,
            edited_at_ms: None,
            revoked: false,
            starred: false,
        }
    }

    #[test]
    fn acknowledged_outgoing_text_is_editable_only_inside_window() {
        let now = 2_000_000;
        assert!(row(now - MESSAGE_EDIT_WINDOW_MS).can_edit_text_at(now));
        assert!(!row(now - MESSAGE_EDIT_WINDOW_MS - 1).can_edit_text_at(now));

        let mut incoming = row(now);
        incoming.direction = MessageDirection::Incoming;
        assert!(!incoming.can_edit_text_at(now));

        let mut pending = row(now);
        pending.status = MessageStatus::Pending;
        assert!(!pending.can_edit_text_at(now));

        let mut media = row(now);
        media.kind = MessageKind::Unknown;
        assert!(!media.can_edit_text_at(now));
    }
}

/// A keyset page of messages, ordered newest→oldest.
#[derive(Clone, Debug)]
pub struct MessagePage {
    pub rows: Vec<MessageRow>,
    /// Cursor of the oldest row; pass back to fetch the page before it.
    /// `None` when the beginning of history is reached.
    pub next_before: Option<PageCursor>,
}

/// A bounded message window centered on one durable search/action target.
/// Rows use the same newest→oldest ordering as [`MessagePage`].
#[derive(Clone, Debug)]
pub struct MessageContext {
    pub rows: Vec<MessageRow>,
    pub anchor: MessageId,
    pub has_more_older: bool,
    pub has_more_newer: bool,
}

/// Opaque pagination cursor handed back by the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageCursor {
    pub timestamp_ms: i64,
    pub seq: LocalCursor,
}

//! Wasabi domain types.
//!
//! Pure, headless, GPUI-free. Everything the UI and the core exchange that is
//! durable-shaped lives here; protocol types never cross this boundary
//!.

pub mod actions;
pub mod conversation;
pub mod error;
pub mod ids;
pub mod media;
pub mod message;
pub mod notification;
pub mod page;
pub mod pairing;
pub mod preferences;
pub mod presence;
pub mod search;
pub mod send;

pub use actions::{ChatAction, MessageAction, MessageActionTarget};
pub use conversation::{
    AvatarRef, ChatKind, ConversationDetails, DirectContactDetails, GroupDetails, GroupPermissions,
    Participant, ParticipantRole,
};
pub use error::{ErrorKind, ServiceError};
pub use ids::{AccountId, ChatId, LocalCursor, MediaId, MessageId, TransferId};
pub use media::{CachedMedia, MediaDownloadRequest, TransferDirection, TransferJob, TransferState};
pub use message::{
    ChatSummary, MediaAvailability, MediaDescriptor, MessageContext, MessageDirection, MessageKind,
    MessagePage, MessageRow, MessageStatus, PageCursor, SenderJid, UnavailableMessageReason,
};
pub use notification::NotificationCandidate;
pub use page::{ChatPage, ChatPageCursor, ChatScope};
pub use pairing::{PairingPhoneNumber, PhonePairCode};
pub use preferences::Draft;
pub use presence::{TypingState, TypingUpdate};
pub use search::{MessageSearchHit, SearchPage};
pub use send::{SendContent, SendReceipt, SendRequest};

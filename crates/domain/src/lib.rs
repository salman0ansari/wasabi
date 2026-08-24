//! Wasabi domain types.
//!
//! Pure, headless, GPUI-free. Everything the UI and the core exchange that is
//! durable-shaped lives here; protocol types never cross this boundary
//!.

pub mod conversation;
pub mod error;
pub mod ids;
pub mod message;
pub mod page;
pub mod preferences;
pub mod search;
pub mod send;

pub use conversation::{
    AvatarRef, ChatKind, ConversationDetails, DirectContactDetails, GroupDetails, GroupPermissions,
    Participant, ParticipantRole,
};
pub use error::{ErrorKind, ServiceError};
pub use ids::{AccountId, ChatId, LocalCursor, MessageId};
pub use message::{
    ChatSummary, MessageDirection, MessageKind, MessagePage, MessageRow, MessageStatus, PageCursor,
    SenderJid,
};
pub use page::{ChatPage, ChatPageCursor, ChatScope};
pub use preferences::Draft;
pub use search::{MessageSearchHit, SearchPage};
pub use send::{SendContent, SendReceipt, SendRequest};

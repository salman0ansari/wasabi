//! Wasabi domain types.
//!
//! Pure, headless, GPUI-free. Everything the UI and the core exchange that is
//! durable-shaped lives here; protocol types never cross this boundary
//!.

pub mod error;
pub mod ids;
pub mod message;
pub mod page;

pub use error::{ErrorKind, ServiceError};
pub use ids::{AccountId, ChatId, LocalCursor, MessageId};
pub use page::{ChatPage, ChatPageCursor};
pub use message::{
    ChatSummary, MessageDirection, MessageKind, MessagePage, MessageRow, MessageStatus, PageCursor,
    SenderJid,
};

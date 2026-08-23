//! UI models over durable state. Every mutation happens in entity update
//! closures guarded by generation counters owned by the views.

mod chats;
mod messages;
mod session;

pub use chats::{ChatFilter, ChatListModel};
pub use messages::{MessageWindowModel, TimelineItem};
pub use session::SessionMirror;

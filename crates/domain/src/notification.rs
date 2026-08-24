//! Product-shaped desktop notification candidates.

use crate::{ChatId, MessageId};

#[derive(Clone, Debug)]
pub struct NotificationCandidate {
    pub chat: ChatId,
    pub message: MessageId,
    pub title: String,
    pub preview: String,
    pub timestamp_ms: i64,
    pub outgoing: bool,
    pub muted: bool,
    pub eligible: bool,
}

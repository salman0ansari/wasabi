//! Ephemeral presence projections. These values are never persisted.

use serde::{Deserialize, Serialize};

use crate::ChatId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypingState {
    Composing,
    RecordingAudio,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypingUpdate {
    pub chat: ChatId,
    /// Sender identity for group conversations. Direct chats leave this
    /// absent because the chat identity already names the peer.
    pub participant: Option<String>,
    pub state: TypingState,
}

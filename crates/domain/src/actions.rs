//! Immutable product commands. Targets capture every identity needed before
//! asynchronous work begins, so a later conversation switch cannot redirect
//! an action.

use serde::{Deserialize, Serialize};

use crate::{ChatId, MessageDirection, MessageId, MessageRow};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageActionTarget {
    pub chat: ChatId,
    pub message: MessageId,
    pub sender: String,
    pub from_me: bool,
    pub timestamp_ms: i64,
}

impl From<&MessageRow> for MessageActionTarget {
    fn from(row: &MessageRow) -> Self {
        Self {
            chat: row.chat.clone(),
            message: row.id.clone(),
            sender: row.sender.bare.clone(),
            from_me: row.direction == MessageDirection::Outgoing,
            timestamp_ms: row.timestamp_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageAction {
    Retry {
        target: MessageActionTarget,
    },
    Star {
        target: MessageActionTarget,
        starred: bool,
    },
    React {
        target: MessageActionTarget,
        emoji: String,
    },
    DeleteForMe {
        target: MessageActionTarget,
        delete_media: bool,
    },
    RevokeForEveryone {
        target: MessageActionTarget,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatAction {
    Pin { chat: ChatId, pinned: bool },
    Mute { chat: ChatId, muted: bool },
    Archive { chat: ChatId, archived: bool },
    MarkRead { chat: ChatId, read: bool },
}

impl ChatAction {
    pub fn chat(&self) -> &ChatId {
        match self {
            Self::Pin { chat, .. }
            | Self::Mute { chat, .. }
            | Self::Archive { chat, .. }
            | Self::MarkRead { chat, .. } => chat,
        }
    }
}

impl MessageAction {
    pub fn target(&self) -> &MessageActionTarget {
        match self {
            Self::Retry { target }
            | Self::Star { target, .. }
            | Self::React { target, .. }
            | Self::DeleteForMe { target, .. }
            | Self::RevokeForEveryone { target } => target,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalCursor, MessageKind, MessageStatus, SenderJid};

    #[test]
    fn target_captures_chat_before_async_dispatch() {
        let row = MessageRow {
            id: MessageId::new("message-a"),
            chat: ChatId::new("chat-a@s.whatsapp.net"),
            direction: MessageDirection::Incoming,
            sender: SenderJid {
                bare: "sender@s.whatsapp.net".to_string(),
                push_name: None,
            },
            timestamp_ms: 42,
            seq: LocalCursor(1),
            kind: MessageKind::Text {
                body: "hello".to_string(),
            },
            status: MessageStatus::Delivered,
            edited_at_ms: None,
            revoked: false,
            starred: false,
        };
        let action = MessageAction::Star {
            target: (&row).into(),
            starred: true,
        };

        assert_eq!(action.target().chat.as_str(), "chat-a@s.whatsapp.net");
        assert_eq!(action.target().message.as_str(), "message-a");
    }

    #[test]
    fn chat_action_captures_destination() {
        let action = ChatAction::Mute {
            chat: ChatId::new("chat-a@s.whatsapp.net"),
            muted: true,
        };
        assert_eq!(action.chat().as_str(), "chat-a@s.whatsapp.net");
    }

    #[test]
    fn retry_captures_the_original_message_identity() {
        let action = MessageAction::Retry {
            target: MessageActionTarget {
                chat: ChatId::new("chat-a@s.whatsapp.net"),
                message: MessageId::new("failed-message"),
                sender: "me@s.whatsapp.net".to_string(),
                from_me: true,
                timestamp_ms: 42,
            },
        };

        assert_eq!(action.target().chat.as_str(), "chat-a@s.whatsapp.net");
        assert_eq!(action.target().message.as_str(), "failed-message");
    }
}

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
    Edit {
        target: MessageActionTarget,
        body: String,
    },
    DeleteForMe {
        target: MessageActionTarget,
        delete_media: bool,
    },
    RevokeForEveryone {
        target: MessageActionTarget,
    },
    Forward {
        target: MessageActionTarget,
        destinations: Vec<ChatId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatAction {
    Pin {
        chat: ChatId,
        pinned: bool,
    },
    Mute {
        chat: ChatId,
        muted: bool,
    },
    Archive {
        chat: ChatId,
        archived: bool,
    },
    MarkRead {
        chat: ChatId,
        read: bool,
    },
    Clear {
        chat: ChatId,
        delete_starred: bool,
        delete_media: bool,
    },
    Delete {
        chat: ChatId,
        delete_media: bool,
    },
}

impl ChatAction {
    pub fn chat(&self) -> &ChatId {
        match self {
            Self::Pin { chat, .. }
            | Self::Mute { chat, .. }
            | Self::Archive { chat, .. }
            | Self::MarkRead { chat, .. }
            | Self::Clear { chat, .. }
            | Self::Delete { chat, .. } => chat,
        }
    }

    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::Clear { .. } | Self::Delete { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactAction {
    Block { jid: ChatId },
    Unblock { jid: ChatId },
    Remove { jid: ChatId },
}

impl ContactAction {
    pub fn jid(&self) -> &ChatId {
        match self {
            Self::Block { jid } | Self::Unblock { jid } | Self::Remove { jid } => jid,
        }
    }
}

impl MessageAction {
    pub fn target(&self) -> &MessageActionTarget {
        match self {
            Self::Retry { target }
            | Self::Star { target, .. }
            | Self::React { target, .. }
            | Self::Edit { target, .. }
            | Self::DeleteForMe { target, .. }
            | Self::RevokeForEveryone { target }
            | Self::Forward { target, .. } => target,
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
            quoted: None,
            reactions: Vec::new(),
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
    fn destructive_chat_actions_capture_scope_and_flags() {
        let clear = ChatAction::Clear {
            chat: ChatId::new("chat-a@s.whatsapp.net"),
            delete_starred: false,
            delete_media: false,
        };
        let delete = ChatAction::Delete {
            chat: ChatId::new("chat-b@g.us"),
            delete_media: true,
        };

        assert!(clear.is_destructive());
        assert_eq!(clear.chat().as_str(), "chat-a@s.whatsapp.net");
        assert!(delete.is_destructive());
        assert_eq!(delete.chat().as_str(), "chat-b@g.us");
        assert!(matches!(
            delete,
            ChatAction::Delete {
                delete_media: true,
                ..
            }
        ));
    }

    #[test]
    fn contact_actions_capture_jid_and_are_not_chat_delete() {
        let jid = ChatId::new("15550000001@s.whatsapp.net");
        let block = ContactAction::Block { jid: jid.clone() };
        let unblock = ContactAction::Unblock { jid: jid.clone() };
        let remove = ContactAction::Remove { jid: jid.clone() };
        let delete_chat = ChatAction::Delete {
            chat: jid.clone(),
            delete_media: false,
        };

        assert_eq!(block.jid().as_str(), "15550000001@s.whatsapp.net");
        assert_eq!(unblock.jid().as_str(), "15550000001@s.whatsapp.net");
        assert_eq!(remove.jid().as_str(), "15550000001@s.whatsapp.net");
        assert!(matches!(block, ContactAction::Block { .. }));
        assert!(matches!(remove, ContactAction::Remove { .. }));
        assert!(matches!(delete_chat, ChatAction::Delete { .. }));
        assert_ne!(
            format!("{remove:?}"),
            format!("{delete_chat:?}"),
            "removing a saved contact is not ChatAction::Delete"
        );
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

    #[test]
    fn edit_captures_body_and_original_destination() {
        let action = MessageAction::Edit {
            target: MessageActionTarget {
                chat: ChatId::new("chat-a@s.whatsapp.net"),
                message: MessageId::new("message-a"),
                sender: "me@s.whatsapp.net".to_string(),
                from_me: true,
                timestamp_ms: 42,
            },
            body: "corrected text".to_string(),
        };

        assert_eq!(action.target().chat.as_str(), "chat-a@s.whatsapp.net");
        assert!(matches!(
            action,
            MessageAction::Edit { body, .. } if body == "corrected text"
        ));
    }

    #[test]
    fn forward_captures_destinations_and_original_target() {
        let action = MessageAction::Forward {
            target: MessageActionTarget {
                chat: ChatId::new("chat-a@s.whatsapp.net"),
                message: MessageId::new("message-a"),
                sender: "me@s.whatsapp.net".to_string(),
                from_me: true,
                timestamp_ms: 42,
            },
            destinations: vec![
                ChatId::new("chat-b@s.whatsapp.net"),
                ChatId::new("120363000000000001@g.us"),
            ],
        };

        assert_eq!(action.target().chat.as_str(), "chat-a@s.whatsapp.net");
        assert_eq!(action.target().message.as_str(), "message-a");
        assert!(matches!(
            action,
            MessageAction::Forward { destinations, .. }
                if destinations
                    == [
                        ChatId::new("chat-b@s.whatsapp.net"),
                        ChatId::new("120363000000000001@g.us")
                    ]
        ));
    }
}

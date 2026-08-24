//! Product-facing outgoing request types.
//!
//! A request captures its destination at creation time. UI selection state is
//! deliberately absent, so a later chat switch cannot redirect an in-flight
//! send. Protocol messages and transport handles stay behind the desktop
//! backend boundary.

use serde::{Deserialize, Serialize};

use crate::{ChatId, MessageId, TransferId};

/// One immutable outgoing operation submitted by the product UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    pub chat: ChatId,
    pub content: SendContent,
}

impl SendRequest {
    pub fn text(chat: ChatId, body: impl Into<String>) -> Self {
        Self {
            chat,
            content: SendContent::Text { body: body.into() },
        }
    }

    pub fn attachment(chat: ChatId, transfer: TransferId, caption: Option<String>) -> Self {
        Self {
            chat,
            content: SendContent::Attachment { transfer, caption },
        }
    }
}

/// Payloads supported by the typed outbox boundary.
///
/// Media variants will be additive here; opaque media handles will keep raw
/// bytes and protocol metadata out of the UI process model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SendContent {
    Text {
        body: String,
    },
    Attachment {
        transfer: TransferId,
        caption: Option<String>,
    },
}

/// Confirmation that the durable send pipeline accepted and published the
/// request under this immutable message identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReceipt {
    pub message: MessageId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_request_captures_destination_and_body() {
        let request = SendRequest::text(ChatId::new("group-a@g.us"), "hello");
        assert_eq!(request.chat.as_str(), "group-a@g.us");
        assert_eq!(
            request.content,
            SendContent::Text {
                body: "hello".to_string()
            }
        );
    }

    #[test]
    fn attachment_request_captures_destination_transfer_and_caption() {
        let request = SendRequest::attachment(
            ChatId::new("chat-a@s.whatsapp.net"),
            TransferId::new("transfer-a"),
            Some("caption".to_string()),
        );
        assert_eq!(request.chat.as_str(), "chat-a@s.whatsapp.net");
        assert_eq!(
            request.content,
            SendContent::Attachment {
                transfer: TransferId::new("transfer-a"),
                caption: Some("caption".to_string()),
            }
        );
    }
}

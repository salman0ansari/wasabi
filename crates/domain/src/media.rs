//! Product-facing media transfer types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ChatId, MediaId};

/// One immutable request to recover a received payload. Both identities are
/// captured before asynchronous work begins so switching chats cannot redirect
/// the transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDownloadRequest {
    pub chat: ChatId,
    pub media: MediaId,
}

/// A verified payload committed to Wasabi's content-addressed cache.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMedia {
    pub media: MediaId,
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_captures_chat_and_opaque_media_identity() {
        let request = MediaDownloadRequest {
            chat: ChatId::new("chat-a@s.whatsapp.net"),
            media: MediaId::new("MESSAGE-A"),
        };
        assert_eq!(request.chat.as_str(), "chat-a@s.whatsapp.net");
        assert_eq!(request.media.as_str(), "MESSAGE-A");
        assert_eq!(format!("{:?}", request.media), "MediaId(<opaque>)");
    }
}

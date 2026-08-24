//! Product-facing media transfer types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ChatId, ErrorKind, MediaId, MessageId, TransferId};

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

/// Whether a durable job moves bytes into or out of the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferDirection {
    IncomingDownload,
    OutgoingUpload,
}

/// Durable transfer lifecycle. Transient failures remain resumable while
/// permanent failures require the user to replace or remove the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferState {
    Staged,
    Queued,
    Running,
    Succeeded,
    FailedRetryable,
    FailedPermanent,
    Cancelled,
}

impl TransferState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::FailedPermanent | Self::Cancelled
        )
    }
}

/// One restart-safe transfer record. Error details are intentionally omitted;
/// only the redacted product error class may be persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferJob {
    pub transfer: TransferId,
    pub chat: ChatId,
    pub message: Option<MessageId>,
    pub direction: TransferDirection,
    pub state: TransferState,
    pub source_path: Option<PathBuf>,
    pub destination_path: Option<PathBuf>,
    pub media_hash: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub error_kind: Option<ErrorKind>,
    pub updated_at_ms: i64,
}

impl TransferJob {
    pub fn staged_upload(
        transfer: TransferId,
        chat: ChatId,
        source_path: PathBuf,
        bytes_total: u64,
    ) -> Self {
        Self {
            transfer,
            chat,
            message: None,
            direction: TransferDirection::OutgoingUpload,
            state: TransferState::Staged,
            source_path: Some(source_path),
            destination_path: None,
            media_hash: None,
            bytes_done: 0,
            bytes_total: Some(bytes_total),
            error_kind: None,
            updated_at_ms: 0,
        }
    }
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

    #[test]
    fn staged_upload_is_restart_shaped_and_not_terminal() {
        let job = TransferJob::staged_upload(
            TransferId::new("transfer-a"),
            ChatId::new("group-a@g.us"),
            PathBuf::from("/tmp/photo.jpg"),
            42,
        );
        assert_eq!(job.direction, TransferDirection::OutgoingUpload);
        assert_eq!(job.state, TransferState::Staged);
        assert!(!job.state.is_terminal());
        assert_eq!(format!("{:?}", job.transfer), "TransferId(<opaque>)");
    }
}

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

/// One immutable request to recover a contact or group profile photo. The
/// identity is captured before asynchronous work begins so switching chats
/// cannot redirect the transfer. `refresh` bypasses a warm disk hit so a
/// PictureUpdate can replace or clear bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilePictureRequest {
    pub jid: ChatId,
    pub refresh: bool,
}

/// A profile photo committed to Wasabi's media cache. The path is a local
/// cache file; remote CDN URLs never cross this boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedAvatar {
    pub jid: ChatId,
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

/// Composer-facing class used to select the correct protocol media builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    Document,
}

/// Durable metadata needed to reconstruct an outgoing attachment after a
/// restart. It contains no media bytes, encryption keys, or remote URLs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPayload {
    pub kind: AttachmentKind,
    pub display_name: String,
    pub mime_type: String,
    pub caption: Option<String>,
    /// Push-to-talk voice note. Absent in older staged jobs.
    #[serde(default)]
    pub voice_note: bool,
    /// Rounded duration for voice notes and other timed audio.
    #[serde(default)]
    pub duration_seconds: Option<u32>,
}

/// Safe composer projection returned after a source has been copied into
/// Wasabi-owned staging and recorded durably.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedAttachment {
    pub transfer: TransferId,
    pub kind: AttachmentKind,
    pub display_name: String,
    pub mime_type: String,
    pub bytes_total: u64,
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
    pub payload: Option<TransferPayload>,
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
            payload: None,
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
    fn profile_picture_request_captures_chat_identity() {
        let request = ProfilePictureRequest {
            jid: ChatId::new("15550000001@s.whatsapp.net"),
            refresh: true,
        };
        assert_eq!(request.jid.as_str(), "15550000001@s.whatsapp.net");
        assert!(request.refresh);
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

    #[test]
    fn transfer_payload_voice_note_defaults_for_legacy_json() {
        let payload: TransferPayload = serde_json::from_str(
            r#"{"kind":"Audio","display_name":"clip.ogg","mime_type":"audio/ogg","caption":null}"#,
        )
        .expect("legacy payload");
        assert!(!payload.voice_note);
        assert_eq!(payload.duration_seconds, None);
    }

    #[test]
    fn transfer_payload_roundtrips_voice_note_metadata() {
        let payload = TransferPayload {
            kind: AttachmentKind::Audio,
            display_name: "Voice message".to_string(),
            mime_type: "audio/ogg; codecs=opus".to_string(),
            caption: None,
            voice_note: true,
            duration_seconds: Some(4),
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        let parsed: TransferPayload = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, payload);
        assert!(parsed.voice_note);
        assert_eq!(parsed.duration_seconds, Some(4));
    }
}

//! Fictitious debug-only fixtures for deterministic visual inspection.

use wasabi_domain::{
    ChatId, ChatKind, ChatSummary, LocalCursor, MediaAvailability, MediaDescriptor, MediaId,
    MessageDirection, MessageId, MessageKind, MessagePage, MessageRow, MessageStatus, Participant,
    ParticipantRole, SenderJid,
};

pub(crate) struct MediaPreview {
    pub chat: ChatId,
    pub summary: ChatSummary,
    pub page: MessagePage,
}

pub(crate) fn media_preview() -> MediaPreview {
    let chat = ChatId::new("preview@s.whatsapp.net");
    let now = chrono::Utc::now().timestamp_millis();
    let descriptor = |id: &str,
                      mime_type: &str,
                      file_name: Option<&str>,
                      file_size: u64,
                      duration_seconds: Option<u32>,
                      width: Option<u32>,
                      height: Option<u32>| MediaDescriptor {
        id: MediaId::new(id),
        mime_type: Some(mime_type.to_string()),
        file_name: file_name.map(str::to_string),
        file_size: Some(file_size),
        duration_seconds,
        width,
        height,
        availability: MediaAvailability::Remote,
    };
    let row = |id: &str, seq: i64, direction: MessageDirection, kind: MessageKind| -> MessageRow {
        MessageRow {
            id: MessageId::new(id),
            chat: chat.clone(),
            direction,
            sender: SenderJid {
                bare: "preview@s.whatsapp.net".to_string(),
                push_name: Some("Avery Chen".to_string()),
            },
            timestamp_ms: now - (6 - seq) * 60_000,
            seq: LocalCursor(seq),
            kind,
            quoted: None,
            reactions: Vec::new(),
            status: MessageStatus::Read,
            edited_at_ms: None,
            revoked: false,
            starred: false,
        }
    };

    MediaPreview {
        chat: chat.clone(),
        summary: ChatSummary {
            id: chat.clone(),
            kind: ChatKind::Direct,
            display_name: Some("Avery Chen".to_string()),
            last_activity_ms: now,
            last_message_preview: Some("Quarterly report.pdf".to_string()),
            unread_count: 0,
            pinned_at_ms: None,
            muted_until_ms: None,
            archived: false,
            favorite: true,
            draft_preview: None,
            draft: None,
            avatar: None,
        },
        page: MessagePage {
            // Repository page order is newest to oldest.
            rows: vec![
                row(
                    "PREVIEW-MULTILINGUAL",
                    6,
                    MessageDirection::Incoming,
                    MessageKind::Text {
                        body: "مرحبا — यह संदेश वास्तविक आकार में मापा जाता है।\n日本語と emoji 🎉 stay readable without overlapping the next bubble."
                            .to_string(),
                    },
                ),
                row(
                    "PREVIEW-MULTILINE",
                    5,
                    MessageDirection::Outgoing,
                    MessageKind::Text {
                        body: "This is a deliberately long desktop message. It wraps from the rendered width instead of a character-count guess.\n\nResizing the window or changing text size asks GPUI to measure the bubble again while the current reading position stays anchored."
                            .to_string(),
                    },
                ),
                row(
                    "PREVIEW-DOC",
                    4,
                    MessageDirection::Incoming,
                    MessageKind::Document {
                        media: descriptor(
                            "PREVIEW-DOC",
                            "application/pdf",
                            Some("Quarterly report.pdf"),
                            2_830_000,
                            None,
                            None,
                            None,
                        ),
                    },
                ),
                row(
                    "PREVIEW-AUDIO",
                    3,
                    MessageDirection::Outgoing,
                    MessageKind::Audio {
                        voice_note: true,
                        media: descriptor(
                            "PREVIEW-AUDIO",
                            "audio/ogg; codecs=opus",
                            None,
                            184_000,
                            Some(42),
                            None,
                            None,
                        ),
                    },
                ),
                row(
                    "PREVIEW-IMAGE",
                    2,
                    MessageDirection::Incoming,
                    MessageKind::Image {
                        caption: Some("The new workspace is coming together.".to_string()),
                        media: descriptor(
                            "PREVIEW-IMAGE",
                            "image/jpeg",
                            None,
                            1_480_000,
                            None,
                            Some(1600),
                            Some(900),
                        ),
                    },
                ),
                row(
                    "PREVIEW-TEXT",
                    1,
                    MessageDirection::Outgoing,
                    MessageKind::Text {
                        body: "Looks great — I’ll review it today.".to_string(),
                    },
                ),
                row(
                    "PREVIEW-VIEW-ONCE",
                    0,
                    MessageDirection::Incoming,
                    MessageKind::Unavailable {
                        reason: wasabi_domain::UnavailableMessageReason::ViewOnceOnPhone,
                    },
                ),
            ],
            next_before: None,
        },
    }
}

pub(crate) fn group_details_preview() -> wasabi_domain::GroupDetails {
    let participants = [
        (
            "You",
            "preview-owner@s.whatsapp.net",
            ParticipantRole::SuperAdmin,
            true,
        ),
        (
            "Avery Chen",
            "preview-avery@s.whatsapp.net",
            ParticipantRole::Admin,
            false,
        ),
        (
            "Amara Okafor",
            "preview-amara@s.whatsapp.net",
            ParticipantRole::Member,
            false,
        ),
        (
            "Diego Morales",
            "preview-diego@s.whatsapp.net",
            ParticipantRole::Member,
            false,
        ),
    ]
    .into_iter()
    .map(|(display_name, jid, role, is_self)| Participant {
        jid: jid.to_string(),
        display_name: display_name.to_string(),
        avatar: None,
        role,
        is_self,
    })
    .collect();

    wasabi_domain::GroupDetails {
        chat: ChatId::new("preview-group@g.us"),
        subject: "Weekend hiking crew".to_string(),
        description: Some("Trail plans, weather checks, and shared packing lists.".to_string()),
        avatar: None,
        participant_count: 4,
        participants,
        permissions: wasabi_domain::GroupPermissions {
            only_admins_edit: true,
            only_admins_send: false,
            membership_approval: true,
            current_user_role: Some(ParticipantRole::SuperAdmin),
        },
    }
}

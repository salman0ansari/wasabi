//! Integration tests for the `AccountStore` facade: real SQLite file, real
//! writer task, events fed exactly as the client would.

// Tests exercise the raw store APIs.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use diesel::prelude::*;
use wacore::proto_helpers::MessageBuilderExt;
use wacore::types::events::{BatchOrigin, Event, InboundMessage, MessageBatch};
use wacore::types::message::{MessageInfo, MessageSource};
use waproto::buffa::MessageField;
use waproto::whatsapp as wa;
use wasabi_domain as domain;
use wasabi_repository::{AccountStore, StoreTuning};
use wasabi_test_support::TestDir;
use whatsapp_rust::Jid;
use whatsapp_rust_chat_store::ChatStore;

const PEER1: &str = "559900000001@s.whatsapp.net";
const PEER2: &str = "559900000002@s.whatsapp.net";

fn jid(s: &str) -> Jid {
    s.parse().expect("valid test JID")
}

async fn open(dir: &TestDir) -> AccountStore {
    AccountStore::open(&dir.path().join("store.sqlite3"), &StoreTuning::default())
        .await
        .expect("open account store")
}

fn enqueue_inbound(chats: &ChatStore, batch: Vec<InboundMessage>) {
    chats.handler().handle_event(Arc::new(Event::Messages(
        MessageBatch::builder()
            .messages(Arc::from(batch))
            .origin(BatchOrigin::Live)
            .build(),
    )));
}

fn incoming_info(chat: &str, sender: &str, id: &str, ts_secs: i64) -> MessageInfo {
    MessageInfo {
        source: MessageSource {
            chat: jid(chat),
            sender: jid(sender),
            is_from_me: false,
            is_group: chat.ends_with("@g.us"),
            ..Default::default()
        },
        id: id.to_string(),
        timestamp: Utc.timestamp_opt(ts_secs, 0).unwrap(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn open_empty_db_yields_empty_pages() {
    let dir = TestDir::new("empty");
    let store = open(&dir).await;

    let chats = store
        .chat_page(domain::ChatScope::Active, None, 50)
        .await
        .unwrap();
    assert!(chats.rows.is_empty());

    let page = store.message_page(PEER1, None, 10).await.unwrap();
    assert!(page.rows.is_empty());
    assert!(page.next_before.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn acknowledged_empty_group_is_durable_without_a_fake_message() {
    let dir = TestDir::new("created-group");
    let store = open(&dir).await;
    let created_at_ms = 1_800_000_000_000;
    store
        .record_created_group(
            domain::ChatId::new("120363000000000001@g.us"),
            "Weekend plans".to_string(),
            created_at_ms,
        )
        .await
        .unwrap();

    let chats = store
        .chat_page(domain::ChatScope::Active, None, 50)
        .await
        .unwrap();
    assert_eq!(chats.rows.len(), 1);
    assert_eq!(chats.rows[0].kind, domain::ChatKind::Group);
    assert_eq!(chats.rows[0].display_name.as_deref(), Some("Weekend plans"));
    assert_eq!(chats.rows[0].last_activity_ms, created_at_ms);
    assert!(chats.rows[0].last_message_preview.is_none());

    let messages = store
        .message_page("120363000000000001@g.us", None, 10)
        .await
        .unwrap();
    assert!(messages.rows.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn group_details_cache_reopens_and_replaces_participants_atomically() {
    let dir = TestDir::new("group-details-cache");
    let group = domain::ChatId::new("120363000000000002@g.us");
    {
        let store = open(&dir).await;
        store
            .save_group_details(
                domain::GroupDetails {
                    chat: group.clone(),
                    subject: "Weekend plans".to_string(),
                    description: Some("Bring water".to_string()),
                    avatar: Some(domain::AvatarRef("group-avatar".to_string())),
                    participant_count: 3,
                    participants: vec![
                        domain::Participant {
                            jid: PEER1.to_string(),
                            display_name: "Zara".to_string(),
                            avatar: None,
                            role: domain::ParticipantRole::Member,
                            is_self: false,
                        },
                        domain::Participant {
                            jid: PEER2.to_string(),
                            display_name: "You".to_string(),
                            avatar: Some(domain::AvatarRef("self-avatar".to_string())),
                            role: domain::ParticipantRole::Admin,
                            is_self: true,
                        },
                    ],
                    permissions: domain::GroupPermissions {
                        only_admins_edit: true,
                        only_admins_send: false,
                        membership_approval: true,
                        current_user_role: Some(domain::ParticipantRole::Admin),
                    },
                },
                1_800_000_000_000,
            )
            .await
            .unwrap();
    }

    let store = open(&dir).await;
    let cached = store
        .cached_group_details(group.as_str())
        .await
        .unwrap()
        .expect("cached group details");
    assert_eq!(cached.subject, "Weekend plans");
    assert_eq!(cached.description.as_deref(), Some("Bring water"));
    assert_eq!(cached.participant_count, 3);
    assert_eq!(cached.participants.len(), 2);
    assert!(cached.participants[0].is_self, "self is sorted first");
    assert_eq!(
        cached.permissions.current_user_role,
        Some(domain::ParticipantRole::Admin)
    );

    let mut replacement = cached;
    replacement.subject = "Updated plans".to_string();
    replacement.participant_count = 1;
    replacement.participants = vec![replacement.participants.remove(0)];
    store
        .save_group_details(replacement, 1_800_000_001_000)
        .await
        .unwrap();
    let updated = store
        .cached_group_details(group.as_str())
        .await
        .unwrap()
        .expect("updated group details");
    assert_eq!(updated.subject, "Updated plans");
    assert_eq!(updated.participants.len(), 1);
    assert!(updated.participants[0].is_self);
}

#[tokio::test(flavor = "multi_thread")]
async fn contact_pages_are_stable_searchable_and_direct_only() {
    let dir = TestDir::new("contact-pagination");
    let store = open(&dir).await;
    store.sqlite().create_new_device().await.unwrap();
    let device_id = store.device_id();
    store
        .shared_db()
        .run(move |connection| {
            let contacts: [(&str, Option<&str>, Option<&str>, Option<&str>, Option<&str>); 6] = [
                ("3@s.whatsapp.net", Some("Charlie"), None, None, None),
                ("1@s.whatsapp.net", None, None, Some("alice"), None),
                ("2@s.whatsapp.net", Some("Bob"), None, None, None),
                (
                    "4@s.whatsapp.net",
                    Some("Percent% Person"),
                    None,
                    None,
                    None,
                ),
                ("not-a-contact@g.us", Some("Not a person"), None, None, None),
                ("222@lid", Some("Bob"), None, None, None),
            ];
            for (jid, push, full, first, business) in contacts {
                diesel::sql_query(
                    "INSERT INTO contacts
                     (device_id, jid, push_name, full_name, first_name, business_name)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::Text, _>(jid)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(push)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(full)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(first)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(business)
                .execute(connection)
                .map_err(|error| wacore::store::error::StoreError::Database(Box::new(error)))?;
            }
            diesel::sql_query(
                "INSERT INTO lid_pn_mapping
                 (lid, phone_number, created_at, learning_source, updated_at, device_id)
                 VALUES ('222', '2', 1, 'test', 1, ?)",
            )
            .bind::<diesel::sql_types::Integer, _>(device_id)
            .execute(connection)
            .map_err(|error| wacore::store::error::StoreError::Database(Box::new(error)))?;
            diesel::sql_query(
                "INSERT INTO wasabi_contact_cache
                 (device_id, jid, display_name, about, avatar_ref, fetched_at_ms)
                 VALUES (?, '2@s.whatsapp.net', NULL, NULL, 'avatar-bob', 1)",
            )
            .bind::<diesel::sql_types::Integer, _>(device_id)
            .execute(connection)
            .map_err(|error| wacore::store::error::StoreError::Database(Box::new(error)))?;
            Ok(())
        })
        .await
        .unwrap();

    let first = store.contact_page(String::new(), None, 2).await.unwrap();
    assert_eq!(
        first
            .rows
            .iter()
            .map(|row| row.display_name.as_str())
            .collect::<Vec<_>>(),
        ["alice", "Bob"]
    );
    assert_eq!(
        first.rows[1]
            .avatar
            .as_ref()
            .map(|avatar| avatar.0.as_str()),
        Some("avatar-bob")
    );
    let second = store
        .contact_page(String::new(), first.next_after, 2)
        .await
        .unwrap();
    assert_eq!(
        second
            .rows
            .iter()
            .map(|row| row.display_name.as_str())
            .collect::<Vec<_>>(),
        ["Charlie", "Percent% Person"]
    );
    assert!(second.next_after.is_none());

    let literal = store.contact_page("%".to_string(), None, 20).await.unwrap();
    assert_eq!(literal.rows.len(), 1, "percent must be searched literally");
    assert_eq!(literal.rows[0].display_name, "Percent% Person");
}

#[tokio::test(flavor = "multi_thread")]
async fn outgoing_roundtrip_via_flush_barrier() {
    let dir = TestDir::new("outgoing");
    let store = open(&dir).await;

    store
        .chats()
        .record_outgoing(
            &jid(PEER1),
            "MSG-A",
            &wa::Message::text("hello world"),
            Utc::now(),
        )
        .unwrap();
    store.flush().await.unwrap();

    let page = store.message_page(PEER1, None, 10).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    let row = &page.rows[0];
    assert_eq!(row.id.as_str(), "MSG-A");
    assert_eq!(row.direction, domain::MessageDirection::Outgoing);
    assert_eq!(row.status, domain::MessageStatus::Pending);
    assert!(
        matches!(&row.kind, domain::MessageKind::Text { body } if body == "hello world"),
        "expected Text kind with the recorded body, got {:?}",
        row.kind
    );

    let chats = store
        .chat_page(domain::ChatScope::Active, None, 50)
        .await
        .unwrap();
    assert_eq!(chats.rows.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn message_pages_project_durable_reaction_aggregates() {
    let dir = TestDir::new("reaction-projection");
    let store = open(&dir).await;
    let reaction = wa::Message {
        reaction_message: MessageField::some(wa::message::ReactionMessage {
            key: MessageField::some(wa::MessageKey {
                id: Some("TARGET-R".to_string()),
                ..Default::default()
            }),
            text: Some("👍".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    enqueue_inbound(
        store.chats(),
        vec![
            InboundMessage::builder()
                .message(Arc::new(wa::Message::text("react to this")))
                .info(Arc::new(incoming_info(
                    PEER1,
                    PEER1,
                    "TARGET-R",
                    1_700_000_000,
                )))
                .build(),
            InboundMessage::builder()
                .message(Arc::new(reaction))
                .info(Arc::new(incoming_info(
                    PEER1,
                    PEER1,
                    "REACTION-R",
                    1_700_000_001,
                )))
                .build(),
        ],
    );
    store.flush().await.unwrap();

    let page = store.message_page(PEER1, None, 10).await.unwrap();
    assert_eq!(
        page.rows.len(),
        1,
        "reaction events are not message bubbles"
    );
    assert_eq!(page.rows[0].reactions.len(), 1);
    assert_eq!(page.rows[0].reactions[0].emoji, "👍");
    assert_eq!(page.rows[0].reactions[0].count, 1);
    assert!(!page.rows[0].reactions[0].reacted_by_me);
}

#[tokio::test(flavor = "multi_thread")]
async fn event_delivery_is_idempotent_under_replay() {
    let dir = TestDir::new("replay");
    let store = open(&dir).await;

    let batch = vec![
        InboundMessage::builder()
            .message(Arc::new(wa::Message::text("ping")))
            .info(Arc::new(incoming_info(PEER2, PEER2, "R1", 1_700_000_000)))
            .build(),
    ];

    enqueue_inbound(store.chats(), batch.clone());
    enqueue_inbound(store.chats(), batch);
    store.flush().await.unwrap();

    let page = store.message_page(PEER2, None, 10).await.unwrap();
    assert_eq!(page.rows.len(), 1, "replay must not duplicate the row");

    let chats = store
        .chat_page(domain::ChatScope::Active, None, 50)
        .await
        .unwrap();
    assert_eq!(chats.rows.len(), 1);
    assert_eq!(
        chats.rows[0].unread_count, 1,
        "replay must not double-badge"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn notification_candidate_projects_latest_committed_incoming_message() {
    let dir = TestDir::new("notification-candidate");
    let store = open(&dir).await;
    enqueue_inbound(
        store.chats(),
        vec![
            InboundMessage::builder()
                .message(Arc::new(wa::Message::text("private preview")))
                .info(Arc::new(incoming_info(
                    PEER2,
                    PEER2,
                    "NOTIFY-1",
                    1_700_000_050,
                )))
                .build(),
        ],
    );
    store.flush().await.unwrap();

    let candidate = store
        .notification_candidate(PEER2)
        .await
        .unwrap()
        .expect("candidate");
    assert_eq!(candidate.message.as_str(), "NOTIFY-1");
    assert_eq!(candidate.preview, "private preview");
    assert!(!candidate.outgoing);
    assert!(!candidate.muted);
    assert!(candidate.eligible);
}

#[tokio::test(flavor = "multi_thread")]
async fn media_projection_keeps_display_metadata_and_hides_transport_secrets() {
    let dir = TestDir::new("media-projection");
    let store = open(&dir).await;
    let image = wa::Message {
        image_message: MessageField::some(wa::message::ImageMessage {
            url: Some("https://cdn.invalid/private".to_string()),
            direct_path: Some("/private/path".to_string()),
            media_key: Some(vec![7; 32]),
            file_sha256: Some(vec![8; 32]),
            file_enc_sha256: Some(vec![9; 32]),
            file_length: Some(12_345),
            mimetype: Some("image/jpeg".to_string()),
            caption: Some("sample photo".to_string()),
            width: Some(1600),
            height: Some(900),
            ..Default::default()
        }),
        ..Default::default()
    };
    store
        .chats()
        .record_outgoing(&jid(PEER1), "MEDIA-1", &image, Utc::now())
        .unwrap();
    store.flush().await.unwrap();

    let page = store.message_page(PEER1, None, 10).await.unwrap();
    let domain::MessageKind::Image { caption, media } = &page.rows[0].kind else {
        panic!("expected image projection")
    };
    assert_eq!(caption.as_deref(), Some("sample photo"));
    assert_eq!(media.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(media.file_size, Some(12_345));
    assert_eq!((media.width, media.height), (Some(1600), Some(900)));
    assert_eq!(media.availability, domain::MediaAvailability::Remote);
    assert_eq!(format!("{media:?}").contains("private/path"), false);
    assert_eq!(format!("{:?}", media.id), "MediaId(<opaque>)");

    let context = store
        .message_context(PEER1, domain::MessageId::new("MEDIA-1"), 2, 2)
        .await
        .unwrap();
    let domain::MessageKind::Image { media, .. } = &context.rows[0].kind else {
        panic!("expected anchored image projection")
    };
    assert_eq!(media.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(media.file_size, Some(12_345));
}

#[tokio::test(flavor = "multi_thread")]
async fn keyset_pagination_no_overlap_no_gap() {
    let dir = TestDir::new("pagination");
    let store = open(&dir).await;

    // One fixed instant for all rows: ordering falls to the seq tiebreak.
    let ts = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    for i in 1..=25 {
        let id = format!("M{i:02}");
        store
            .chats()
            .record_outgoing(&jid(PEER1), id, &wa::Message::text("m"), ts)
            .unwrap();
    }
    store.flush().await.unwrap();

    let mut ids = Vec::new();
    let mut keys = Vec::new();
    let mut before: Option<domain::PageCursor> = None;
    loop {
        let page = store.message_page(PEER1, before, 10).await.unwrap();
        assert!(page.rows.len() <= 10);
        for row in &page.rows {
            ids.push(row.id.as_str().to_owned());
            keys.push((row.timestamp_ms, row.seq.0));
        }
        before = match page.next_before {
            Some(cursor) => Some(cursor),
            None => break,
        };
    }

    assert_eq!(ids.len(), 25, "expected every message exactly once");
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        25,
        "no duplicates across pages"
    );
    for pair in keys.windows(2) {
        assert!(
            pair[0] > pair[1],
            "ordering must be strictly descending by (timestamp_ms, seq): {keys:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn anchored_context_returns_exact_bounded_neighbors() {
    let dir = TestDir::new("anchored-context");
    let store = open(&dir).await;
    let ts = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    for i in 1..=25 {
        let id = format!("M{i:02}");
        store
            .chats()
            .record_outgoing(
                &jid(PEER1),
                id,
                &wa::Message::text(format!("message {i}")),
                ts,
            )
            .unwrap();
    }
    store.flush().await.unwrap();

    let context = store
        .message_context(PEER1, domain::MessageId::new("M13"), 3, 2)
        .await
        .unwrap();
    let ids = context
        .rows
        .iter()
        .map(|row| row.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["M15", "M14", "M13", "M12", "M11", "M10"]);
    assert_eq!(context.anchor.as_str(), "M13");
    assert!(context.has_more_older);
    assert!(context.has_more_newer);
}

#[tokio::test(flavor = "multi_thread")]
async fn store_change_invalidation_emitted_after_commit() {
    let dir = TestDir::new("invalidation");
    let store = open(&dir).await;
    let mut rx = store.subscribe_changes();

    let batch = vec![
        InboundMessage::builder()
            .message(Arc::new(wa::Message::text("hi")))
            .info(Arc::new(incoming_info(PEER2, PEER2, "R2", 1_700_000_001)))
            .build(),
    ];
    enqueue_inbound(store.chats(), batch);

    let change = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("change within 2s")
        .expect("channel open");
    assert!(
        matches!(
            change,
            wasabi_repository::StoreChange::Chats | wasabi_repository::StoreChange::Messages { .. }
        ),
        "unexpected change: {change:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reopen_persists_data() {
    let dir = TestDir::new("reopen");
    {
        let store = open(&dir).await;
        store
            .chats()
            .record_outgoing(
                &jid(PEER1),
                "MSG-P",
                &wa::Message::text("durable"),
                Utc::now(),
            )
            .unwrap();
        store.flush().await.unwrap();
    }

    let reopened = open(&dir).await;
    let page = reopened.message_page(PEER1, None, 10).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].id.as_str(), "MSG-P");
}

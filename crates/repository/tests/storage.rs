//! Integration tests for the `AccountStore` facade: real SQLite file, real
//! writer task, events fed exactly as the client would.

// Tests exercise the raw store APIs.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use wacore::proto_helpers::MessageBuilderExt;
use wacore::types::events::{BatchOrigin, Event, InboundMessage, MessageBatch};
use wacore::types::message::{MessageInfo, MessageSource};
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

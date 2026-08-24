//! Integration tests for the upstream chat-store write and event-handler APIs.

#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use chrono::Utc;
use wacore::proto_helpers::MessageBuilderExt;
use wacore::types::events::{BatchOrigin, Event, InboundMessage, MessageBatch};
use wacore::types::message::{MessageInfo, MessageSource};
use waproto::whatsapp as wa;
use wasabi_repository::{AccountStore, StoreTuning};
use wasabi_test_support::TestDir;
use whatsapp_rust::Jid;

const PEER: &str = "559900000001@s.whatsapp.net";

fn jid(s: &str) -> Jid {
    s.parse().expect("valid test JID")
}

async fn open(dir: &TestDir) -> AccountStore {
    AccountStore::open(&dir.path().join("store.sqlite3"), &StoreTuning::default())
        .await
        .expect("open account store")
}

fn inbound(id: &str) -> InboundMessage {
    let info = MessageInfo {
        source: MessageSource {
            chat: jid(PEER),
            sender: jid(PEER),
            is_from_me: false,
            ..Default::default()
        },
        id: id.to_string(),
        timestamp: Utc::now(),
        ..Default::default()
    };
    InboundMessage::builder()
        .message(Arc::new(wa::Message::text("inbound")))
        .info(Arc::new(info))
        .build()
}

fn enqueue_inbound(store: &AccountStore, batch: Vec<InboundMessage>) {
    store
        .chats()
        .handler()
        .handle_event(Arc::new(Event::Messages(
            MessageBatch::builder()
                .messages(Arc::from(batch))
                .origin(BatchOrigin::Live)
                .build(),
        )));
}

#[tokio::test(flavor = "multi_thread")]
async fn public_writer_operations_commit_through_flush() {
    let dir = TestDir::new("writer");
    let store = open(&dir).await;

    store
        .chats()
        .record_outgoing(
            &jid(PEER),
            "OUT-1",
            &wa::Message::text("outgoing"),
            Utc::now(),
        )
        .expect("enqueue outgoing");
    enqueue_inbound(&store, vec![inbound("IN-1")]);
    store.flush().await.expect("flush writes");

    let page = store.message_page(PEER, None, 10).await.unwrap();
    assert_eq!(page.rows.len(), 2);
    assert!(page.rows.iter().any(|row| row.id.as_str() == "OUT-1"));
    assert!(page.rows.iter().any(|row| row.id.as_str() == "IN-1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_event_delivery_is_idempotent() {
    let dir = TestDir::new("replay");
    let store = open(&dir).await;
    let batch = vec![inbound("REPLAY-1")];

    enqueue_inbound(&store, batch.clone());
    enqueue_inbound(&store, batch);
    store.flush().await.expect("flush replay");

    let page = store.message_page(PEER, None, 10).await.unwrap();
    assert_eq!(page.rows.len(), 1, "replay must not duplicate the row");
    assert_eq!(page.rows[0].id.as_str(), "REPLAY-1");
}

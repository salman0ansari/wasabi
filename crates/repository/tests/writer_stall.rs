//! Integration tests: writer ingress must stay bounded while the single
//! SQLite write permit is held by a stalled operation.

#![allow(clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use wacore::proto_helpers::MessageBuilderExt;
use wacore::types::events::{BatchOrigin, Event, InboundMessage, MessageBatch};
use wacore::types::message::{MessageInfo, MessageSource};
use waproto::whatsapp as wa;
use wasabi_repository::{AccountStore, StoreTuning};
use wasabi_test_support::TestDir;
use whatsapp_rust::Jid;
use whatsapp_rust_chat_store::ChatStoreError;

const PEER: &str = "559900000001@s.whatsapp.net";
const CAPACITY: usize = 8192;
const FED: usize = 9000;

fn jid(s: &str) -> Jid {
    s.parse().expect("valid test JID")
}

async fn open(dir: &TestDir) -> AccountStore {
    AccountStore::open(&dir.path().join("store.sqlite3"), &StoreTuning::default())
        .await
        .expect("open account store")
}

/// Occupy the store's single write permit with a blocking sleep; await the
/// returned handle to let the stall finish.
async fn spawn_stall(
    shared: whatsapp_rust_sqlite_storage::SharedSqlite,
    secs: u64,
) -> tokio::task::JoinHandle<Result<(), wacore::store::error::StoreError>> {
    tokio::spawn(async move {
        shared
            .run(move |_conn| {
                std::thread::sleep(Duration::from_secs(secs));
                Ok(())
            })
            .await
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_writer_bounds_ingress_and_counts_drops() {
    let dir = TestDir::new("stall");
    let store = open(&dir).await;
    let chats = store.chats().clone();

    let stall = spawn_stall(store.shared_db(), 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut errors = 0usize;
    for i in 0..FED {
        match chats.record_revoke(&jid(PEER), &format!("T{i}"), Utc::now()) {
            Ok(()) => {}
            Err(ChatStoreError::IngressFull) => errors += 1,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    // The writer may absorb a bounded in-flight batch (<= BATCH_MAX) before
    // parking on the permit, so refusal counts are racy by design; what must
    // hold exactly is: refusals happened and nothing exceeded capacity.
    assert!(errors > 0, "a stalled writer must refuse work once full");
    assert!(errors <= FED - CAPACITY + 128);
    assert!(chats.ingress_depth() <= CAPACITY);

    // The drop counter tracks event-handler refusals; drive it through that
    // path while the queue is still full.
    assert_eq!(chats.ingress_dropped(), 0);
    let handler = chats.handler();
    for i in 0..3 {
        let info = MessageInfo {
            source: MessageSource {
                chat: jid(PEER),
                sender: jid(PEER),
                is_from_me: false,
                ..Default::default()
            },
            id: format!("D{i}"),
            timestamp: Utc::now(),
            ..Default::default()
        };
        let inbound = InboundMessage::builder()
            .message(Arc::new(wa::Message::text("drop")))
            .info(Arc::new(info))
            .build();
        let event = Event::Messages(
            MessageBatch::builder()
                .messages(Arc::from(vec![inbound]))
                .origin(BatchOrigin::Live)
                .build(),
        );
        handler.handle_event(Arc::new(event));
    }
    assert_eq!(chats.ingress_dropped(), 3);

    stall
        .await
        .expect("stall task joins")
        .expect("stall writes");

    chats.flush().await.expect("flush after stall");
    assert!(
        chats.ingress_depth() < 128,
        "queue must drain after flush, depth={}",
        chats.ingress_depth()
    );

    chats
        .record_revoke(&jid(PEER), "recovered", Utc::now())
        .expect("ingress accepts work again");
    chats.flush().await.expect("final flush");
}

#[tokio::test(flavor = "multi_thread")]
async fn async_variant_backpressures_instead_of_erroring() {
    let dir = TestDir::new("stall-async");
    let store = open(&dir).await;
    let chats = store.chats().clone();

    let stall = spawn_stall(store.shared_db(), 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Fill until the queue refuses: capacity is the bound.
    let mut refused = 0usize;
    for i in 0..(CAPACITY * 2) {
        match chats.record_revoke(&jid(PEER), &format!("F{i}"), Utc::now()) {
            Ok(()) => {}
            Err(ChatStoreError::IngressFull) => {
                refused += 1;
                break;
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(refused, 1, "queue must fill to capacity then refuse");
    assert!(chats.ingress_depth() <= CAPACITY);

    tokio::time::timeout(
        Duration::from_secs(10),
        chats.record_outgoing_async(&jid(PEER), "BP1", &wa::Message::text("bp"), Utc::now()),
    )
    .await
    .expect("backpressured send resolves well within the timeout")
    .expect("backpressured send succeeds instead of erroring");

    stall
        .await
        .expect("stall task joins")
        .expect("stall writes");
    store.flush().await.expect("flush");

    let page = store.message_page(PEER, None, 100).await.unwrap();
    assert!(
        page.rows.iter().any(|r| r.id.as_str() == "BP1"),
        "message row must be visible after flush"
    );
}

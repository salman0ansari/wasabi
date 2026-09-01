//! Starred-message keyset query: durable `messages.starred = 1` only.

#![allow(clippy::disallowed_methods)]

use diesel::prelude::*;
use wacore::store::error::StoreError;
use wasabi_domain as domain;
use wasabi_repository::{AccountStore, StoreTuning};
use wasabi_test_support::TestDir;

const PEER1: &str = "559900000001@s.whatsapp.net";
const PEER2: &str = "559900000002@s.whatsapp.net";
const GROUP: &str = "120363000000000021@g.us";

async fn open(dir: &TestDir) -> AccountStore {
    AccountStore::open(&dir.path().join("store.sqlite3"), &StoreTuning::default())
        .await
        .expect("open account store")
}

fn insert_chat(
    connection: &mut diesel::sqlite::SqliteConnection,
    device_id: i32,
    jid: &str,
    name: Option<&str>,
) -> Result<(), StoreError> {
    diesel::sql_query(
        "INSERT INTO chats (device_id, jid, name, last_message_ts)
         VALUES (?, ?, ?, 0)",
    )
    .bind::<diesel::sql_types::Integer, _>(device_id)
    .bind::<diesel::sql_types::Text, _>(jid)
    .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(name)
    .execute(connection)
    .map(|_| ())
    .map_err(|error| StoreError::Database(Box::new(error)))
}

struct MessageSeed {
    chat: &'static str,
    id: &'static str,
    sender: &'static str,
    timestamp_ms: i64,
    kind: &'static str,
    text: Option<&'static str>,
    starred: bool,
    revoked: bool,
}

fn text_msg(
    chat: &'static str,
    id: &'static str,
    timestamp_ms: i64,
    text: &'static str,
    starred: bool,
    revoked: bool,
) -> MessageSeed {
    MessageSeed {
        chat,
        id,
        sender: chat,
        timestamp_ms,
        kind: "text",
        text: Some(text),
        starred,
        revoked,
    }
}

fn insert_message(
    connection: &mut diesel::sqlite::SqliteConnection,
    device_id: i32,
    seed: MessageSeed,
) -> Result<(), StoreError> {
    diesel::sql_query(
        "INSERT INTO messages
         (device_id, chat_jid, msg_id, sender_jid, from_me, timestamp_ms, kind,
          text_content, status, starred, revoked)
         VALUES (?, ?, ?, ?, 0, ?, ?, ?, 2, ?, ?)",
    )
    .bind::<diesel::sql_types::Integer, _>(device_id)
    .bind::<diesel::sql_types::Text, _>(seed.chat)
    .bind::<diesel::sql_types::Text, _>(seed.id)
    .bind::<diesel::sql_types::Text, _>(seed.sender)
    .bind::<diesel::sql_types::BigInt, _>(seed.timestamp_ms)
    .bind::<diesel::sql_types::Text, _>(seed.kind)
    .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(seed.text)
    .bind::<diesel::sql_types::Bool, _>(seed.starred)
    .bind::<diesel::sql_types::Bool, _>(seed.revoked)
    .execute(connection)
    .map(|_| ())
    .map_err(|error| StoreError::Database(Box::new(error)))
}

#[tokio::test(flavor = "multi_thread")]
async fn starred_page_returns_only_starred_newest_first_and_pages() {
    let dir = TestDir::new("starred-page");
    let store = open(&dir).await;
    let device_id = store.device_id();

    store
        .shared_db()
        .run(move |connection| {
            insert_chat(connection, device_id, PEER1, Some("Ada Lovelace"))?;
            insert_chat(connection, device_id, PEER2, None)?;
            insert_chat(connection, device_id, GROUP, Some("Weekend plans"))?;
            insert_message(
                connection,
                device_id,
                text_msg(
                    PEER1,
                    "PLAIN",
                    1_700_000_000_100,
                    "not starred",
                    false,
                    false,
                ),
            )?;
            insert_message(
                connection,
                device_id,
                text_msg(
                    PEER1,
                    "REVOKED",
                    1_700_000_000_900,
                    "was starred",
                    true,
                    true,
                ),
            )?;
            insert_message(
                connection,
                device_id,
                text_msg(
                    PEER1,
                    "OLD",
                    1_700_000_000_100,
                    "oldest starred",
                    true,
                    false,
                ),
            )?;
            insert_message(
                connection,
                device_id,
                text_msg(
                    PEER2,
                    "MID",
                    1_700_000_000_200,
                    "middle starred",
                    true,
                    false,
                ),
            )?;
            insert_message(
                connection,
                device_id,
                MessageSeed {
                    chat: GROUP,
                    id: "PHOTO",
                    sender: PEER1,
                    timestamp_ms: 1_700_000_000_400,
                    kind: "image",
                    text: Some("trail photo"),
                    starred: true,
                    revoked: false,
                },
            )?;
            insert_message(
                connection,
                device_id,
                text_msg(
                    PEER1,
                    "NEW",
                    1_700_000_000_500,
                    "newest starred",
                    true,
                    false,
                ),
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let first = store.starred_messages(None, 2).await.unwrap();
    assert_eq!(first.hits.len(), 2);
    assert!(first.has_more);
    assert!(first.next_after.is_some());
    assert_eq!(first.hits[0].row.id.as_str(), "NEW");
    assert_eq!(first.hits[0].chat_name, "Ada Lovelace");
    assert!(
        matches!(&first.hits[0].row.kind, domain::MessageKind::Text { body } if body == "newest starred")
    );
    assert!(first.hits[0].row.starred);
    assert!(!first.hits[0].row.revoked);
    assert_eq!(first.hits[1].row.id.as_str(), "PHOTO");
    assert_eq!(first.hits[1].chat_name, "Weekend plans");
    assert!(
        matches!(&first.hits[1].row.kind, domain::MessageKind::Image { caption, .. } if caption.as_deref() == Some("trail photo"))
    );

    let second = store.starred_messages(first.next_after, 2).await.unwrap();
    assert_eq!(second.hits.len(), 2);
    assert!(!second.has_more);
    assert!(second.next_after.is_none());
    assert_eq!(second.hits[0].row.id.as_str(), "MID");
    assert_eq!(second.hits[0].chat_name, "559900000002");
    assert_eq!(second.hits[1].row.id.as_str(), "OLD");
    let ids: Vec<&str> = first
        .hits
        .iter()
        .chain(second.hits.iter())
        .map(|hit| hit.row.id.as_str())
        .collect();
    assert_eq!(ids, ["NEW", "PHOTO", "MID", "OLD"]);
    assert!(
        ids.iter().all(|id| *id != "PLAIN" && *id != "REVOKED"),
        "unstarred and revoked rows must not appear"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn starred_page_same_timestamp_uses_rowid_desc() {
    let dir = TestDir::new("starred-rowid-tie");
    let store = open(&dir).await;
    let device_id = store.device_id();
    store
        .shared_db()
        .run(move |connection| {
            insert_chat(connection, device_id, PEER1, Some("Ada Lovelace"))?;
            insert_message(
                connection,
                device_id,
                text_msg(
                    PEER1,
                    "FIRST",
                    1_700_000_000_000,
                    "inserted first",
                    true,
                    false,
                ),
            )?;
            insert_message(
                connection,
                device_id,
                text_msg(
                    PEER1,
                    "SECOND",
                    1_700_000_000_000,
                    "inserted second",
                    true,
                    false,
                ),
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let page = store.starred_messages(None, 10).await.unwrap();
    assert!(!page.has_more);
    assert_eq!(
        page.hits
            .iter()
            .map(|hit| hit.row.id.as_str())
            .collect::<Vec<_>>(),
        ["SECOND", "FIRST"]
    );
    assert!(page.hits[0].row.seq.0 > page.hits[1].row.seq.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_starred_page_has_no_more() {
    let dir = TestDir::new("starred-empty");
    let store = open(&dir).await;
    let page = store.starred_messages(None, 20).await.unwrap();
    assert!(page.hits.is_empty());
    assert!(!page.has_more);
    assert!(page.next_after.is_none());
}

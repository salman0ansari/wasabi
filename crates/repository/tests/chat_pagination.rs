use diesel::prelude::*;
use wacore::store::error::StoreError;
use wasabi_domain::{ChatPageCursor, ChatScope};
use wasabi_repository::{AccountStore, StoreTuning};

#[derive(QueryableByName)]
struct QueryPlanRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    detail: String,
}

#[tokio::test(flavor = "multi_thread")]
async fn active_chat_pages_use_filtered_keyset_index_on_archived_heavy_history() {
    let directory = tempfile::tempdir().unwrap();
    let store = AccountStore::open(
        &directory.path().join("account.db"),
        &StoreTuning::default(),
    )
    .await
    .unwrap();
    let device_id = store.device_id();

    store
        .shared_db()
        .run(move |connection| {
            // Worst case for the generic activity index: thousands of newer
            // archived rows precede the active rows we want.
            diesel::sql_query(
                "WITH RECURSIVE n(x) AS (
                    SELECT 1
                    UNION ALL SELECT x + 1 FROM n WHERE x < 4096
                )
                INSERT INTO chats (device_id, jid, last_message_ts, archived)
                SELECT ?, printf('%05d@s.whatsapp.net', x), 100000 + x, 1 FROM n",
            )
            .bind::<diesel::sql_types::Integer, _>(device_id)
            .execute(connection)
            .map_err(|error| StoreError::Database(Box::new(error)))?;
            diesel::sql_query(
                "WITH RECURSIVE n(x) AS (
                    SELECT 1
                    UNION ALL SELECT x + 1 FROM n WHERE x < 40
                )
                INSERT INTO chats (device_id, jid, last_message_ts, archived)
                SELECT ?, printf('9%04d@s.whatsapp.net', x), 50000 + x, 0 FROM n",
            )
            .bind::<diesel::sql_types::Integer, _>(device_id)
            .execute(connection)
            .map_err(|error| StoreError::Database(Box::new(error)))?;
            Ok(())
        })
        .await
        .unwrap();

    let first = store.chat_page(ChatScope::Active, None, 16).await.unwrap();
    assert_eq!(first.rows.len(), 16);
    assert!(first.rows.iter().all(|chat| !chat.archived));
    assert_eq!(first.rows[0].last_activity_ms, 50_040);
    assert_eq!(first.rows[15].last_activity_ms, 50_025);

    let cursor = first.next_after.clone().expect("second active page");
    let second = store
        .chat_page(ChatScope::Active, Some(cursor.clone()), 16)
        .await
        .unwrap();
    assert_eq!(second.rows.len(), 16);
    assert_eq!(second.rows[0].last_activity_ms, 50_024);
    assert_eq!(second.rows[15].last_activity_ms, 50_009);
    assert!(
        first
            .rows
            .iter()
            .all(|left| second.rows.iter().all(|right| left.id != right.id)),
        "keyset pages must not overlap"
    );

    assert_active_query_plan_uses_wasabi_index(&store, None).await;
    assert_active_query_plan_uses_wasabi_index(&store, Some(cursor)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn archived_chat_pages_skip_newer_active_history_and_preserve_two_run_cursor_order() {
    let directory = tempfile::tempdir().unwrap();
    let store = AccountStore::open(
        &directory.path().join("account.db"),
        &StoreTuning::default(),
    )
    .await
    .unwrap();
    let device_id = store.device_id();

    store
        .shared_db()
        .run(move |connection| {
            diesel::sql_query(
                "WITH RECURSIVE n(x) AS (
                    SELECT 1
                    UNION ALL SELECT x + 1 FROM n WHERE x < 4096
                )
                INSERT INTO chats (device_id, jid, last_message_ts, archived)
                SELECT ?, printf('a%05d@s.whatsapp.net', x), 200000 + x, 0 FROM n",
            )
            .bind::<diesel::sql_types::Integer, _>(device_id)
            .execute(connection)
            .map_err(|error| StoreError::Database(Box::new(error)))?;
            diesel::sql_query(
                "WITH RECURSIVE n(x) AS (
                    SELECT 1
                    UNION ALL SELECT x + 1 FROM n WHERE x < 40
                )
                INSERT INTO chats (device_id, jid, last_message_ts, archived)
                SELECT ?, printf('z%05d@s.whatsapp.net', x), 100000 + x, 1 FROM n",
            )
            .bind::<diesel::sql_types::Integer, _>(device_id)
            .execute(connection)
            .map_err(|error| StoreError::Database(Box::new(error)))?;
            for (jid, activity, pinned_at) in [
                ("p004@s.whatsapp.net", 100_i64, 3_000_i64),
                ("p003@s.whatsapp.net", 200, 2_000),
                ("p002@s.whatsapp.net", 150, 2_000),
                ("p001@s.whatsapp.net", 150, 2_000),
            ] {
                diesel::sql_query(
                    "INSERT INTO chats
                     (device_id, jid, last_message_ts, archived, pinned_at)
                     VALUES (?, ?, ?, 1, ?)",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::Text, _>(jid)
                .bind::<diesel::sql_types::BigInt, _>(activity)
                .bind::<diesel::sql_types::BigInt, _>(pinned_at)
                .execute(connection)
                .map_err(|error| StoreError::Database(Box::new(error)))?;
            }
            Ok(())
        })
        .await
        .unwrap();

    let mut after = None;
    let mut ids = Vec::new();
    let mut pinned_cursor = None;
    let mut activity_cursor = None;
    loop {
        let page = store
            .chat_page(ChatScope::Archived, after.clone(), 3)
            .await
            .unwrap();
        assert!(page.rows.len() <= 3);
        assert!(page.rows.iter().all(|chat| chat.archived));
        for row in &page.rows {
            ids.push(row.id.as_str().to_string());
        }
        if pinned_cursor.is_none()
            && page
                .next_after
                .as_ref()
                .is_some_and(|cursor| cursor.pinned_at_ms.is_some())
        {
            pinned_cursor = page.next_after.clone();
        }
        if activity_cursor.is_none()
            && page
                .next_after
                .as_ref()
                .is_some_and(|cursor| cursor.pinned_at_ms.is_none())
        {
            activity_cursor = page.next_after.clone();
        }
        match page.next_after {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }

    assert_eq!(ids.len(), 44, "every archived chat appears exactly once");
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        44,
        "archived pages must not overlap"
    );
    assert_eq!(
        &ids[..4],
        [
            "p004@s.whatsapp.net",
            "p003@s.whatsapp.net",
            "p002@s.whatsapp.net",
            "p001@s.whatsapp.net",
        ],
        "pinned run sorts by pin time, then activity, then jid"
    );
    assert_eq!(ids[4], "z00040@s.whatsapp.net");
    assert_eq!(ids[43], "z00001@s.whatsapp.net");

    assert_archived_pinned_query_plan_uses_wasabi_index(&store, None).await;
    assert_archived_pinned_query_plan_uses_wasabi_index(
        &store,
        Some(pinned_cursor.expect("cursor within pinned archived run")),
    )
    .await;
    assert_archived_activity_query_plan_uses_wasabi_index(&store, None).await;
    assert_archived_activity_query_plan_uses_wasabi_index(
        &store,
        Some(activity_cursor.expect("cursor within archived activity run")),
    )
    .await;
}

async fn assert_active_query_plan_uses_wasabi_index(
    store: &AccountStore,
    after: Option<ChatPageCursor>,
) {
    let device_id = store.device_id();
    let plan = store
        .shared_db()
        .read(move |connection| {
            let rows = match after {
                Some(after) => diesel::sql_query(
                    "EXPLAIN QUERY PLAN
                     SELECT device_id, jid, name, last_message_ts, last_message_preview,
                            last_message_kind, unread_count, pinned_at, muted_until, archived,
                            ephemeral_expiration, read_boundary_ms, read_boundary_ids
                     FROM chats
                     WHERE device_id = ? AND pinned_at IS NULL AND archived = ?
                       AND (last_message_ts < ? OR (last_message_ts = ? AND jid < ?))
                     ORDER BY last_message_ts DESC, jid DESC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::Bool, _>(false)
                .bind::<diesel::sql_types::BigInt, _>(after.last_activity_ms)
                .bind::<diesel::sql_types::BigInt, _>(after.last_activity_ms)
                .bind::<diesel::sql_types::Text, _>(after.chat.as_str())
                .bind::<diesel::sql_types::BigInt, _>(17_i64)
                .load::<QueryPlanRow>(connection),
                None => diesel::sql_query(
                    "EXPLAIN QUERY PLAN
                     SELECT device_id, jid, name, last_message_ts, last_message_preview,
                            last_message_kind, unread_count, pinned_at, muted_until, archived,
                            ephemeral_expiration, read_boundary_ms, read_boundary_ids
                     FROM chats
                     WHERE device_id = ? AND pinned_at IS NULL AND archived = ?
                     ORDER BY last_message_ts DESC, jid DESC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::Bool, _>(false)
                .bind::<diesel::sql_types::BigInt, _>(17_i64)
                .load::<QueryPlanRow>(connection),
            }
            .map_err(|error| StoreError::Database(Box::new(error)))?;
            Ok(rows.into_iter().map(|row| row.detail).collect::<Vec<_>>())
        })
        .await
        .unwrap();

    assert!(
        plan.iter()
            .any(|detail| detail.contains("wasabi_chats_active_order")),
        "active page should use the filtered keyset index, plan: {plan:?}"
    );
    assert!(
        plan.iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE")),
        "active page must stream in index order, plan: {plan:?}"
    );
}

async fn assert_archived_pinned_query_plan_uses_wasabi_index(
    store: &AccountStore,
    after: Option<ChatPageCursor>,
) {
    let device_id = store.device_id();
    let plan = store
        .shared_db()
        .read(move |connection| {
            let rows = match after {
                Some(after) => diesel::sql_query(
                    "EXPLAIN QUERY PLAN
                     SELECT jid, name, last_message_ts, last_message_preview,
                            unread_count, pinned_at, muted_until
                     FROM chats
                     WHERE device_id = ? AND archived = 1 AND pinned_at IS NOT NULL
                       AND (pinned_at, last_message_ts, jid) < (?, ?, ?)
                     ORDER BY pinned_at DESC, last_message_ts DESC, jid DESC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::BigInt, _>(after.pinned_at_ms.expect("pinned cursor"))
                .bind::<diesel::sql_types::BigInt, _>(after.last_activity_ms)
                .bind::<diesel::sql_types::Text, _>(after.chat.as_str())
                .bind::<diesel::sql_types::BigInt, _>(4_i64)
                .load::<QueryPlanRow>(connection),
                None => diesel::sql_query(
                    "EXPLAIN QUERY PLAN
                     SELECT jid, name, last_message_ts, last_message_preview,
                            unread_count, pinned_at, muted_until
                     FROM chats
                     WHERE device_id = ? AND archived = 1 AND pinned_at IS NOT NULL
                     ORDER BY pinned_at DESC, last_message_ts DESC, jid DESC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::BigInt, _>(4_i64)
                .load::<QueryPlanRow>(connection),
            };
            rows.map(|rows| rows.into_iter().map(|row| row.detail).collect::<Vec<_>>())
                .map_err(|error| StoreError::Database(Box::new(error)))
        })
        .await
        .unwrap();

    assert_plan_uses_index(&plan, "wasabi_chats_archived_pinned", "archived pinned");
}

async fn assert_archived_activity_query_plan_uses_wasabi_index(
    store: &AccountStore,
    after: Option<ChatPageCursor>,
) {
    let device_id = store.device_id();
    let plan = store
        .shared_db()
        .read(move |connection| {
            let rows = match after {
                Some(after) => diesel::sql_query(
                    "EXPLAIN QUERY PLAN
                     SELECT jid, name, last_message_ts, last_message_preview,
                            unread_count, pinned_at, muted_until
                     FROM chats
                     WHERE device_id = ? AND archived = 1 AND pinned_at IS NULL
                       AND (last_message_ts, jid) < (?, ?)
                     ORDER BY last_message_ts DESC, jid DESC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::BigInt, _>(after.last_activity_ms)
                .bind::<diesel::sql_types::Text, _>(after.chat.as_str())
                .bind::<diesel::sql_types::BigInt, _>(4_i64)
                .load::<QueryPlanRow>(connection),
                None => diesel::sql_query(
                    "EXPLAIN QUERY PLAN
                     SELECT jid, name, last_message_ts, last_message_preview,
                            unread_count, pinned_at, muted_until
                     FROM chats
                     WHERE device_id = ? AND archived = 1 AND pinned_at IS NULL
                     ORDER BY last_message_ts DESC, jid DESC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::BigInt, _>(4_i64)
                .load::<QueryPlanRow>(connection),
            }
            .map_err(|error| StoreError::Database(Box::new(error)))?;
            Ok(rows.into_iter().map(|row| row.detail).collect::<Vec<_>>())
        })
        .await
        .unwrap();

    assert_plan_uses_index(&plan, "wasabi_chats_archived_order", "archived activity");
}

fn assert_plan_uses_index(plan: &[String], index: &str, label: &str) {
    assert!(
        plan.iter().any(|detail| detail.contains(index)),
        "{label} page should use {index}, plan: {plan:?}"
    );
    assert!(
        plan.iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE")),
        "{label} page must stream in index order, plan: {plan:?}"
    );
}

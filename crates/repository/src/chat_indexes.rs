//! Wasabi-owned indexes for product query shapes that are narrower than the
//! upstream chat-store's generic list API.

use diesel::prelude::*;
use wacore::store::error::StoreError;
use whatsapp_rust_sqlite_storage::SharedSqlite;

use wasabi_domain as domain;

/// The upstream chat list index intentionally omits `archived` so its
/// active+archived feed can remain one activity-ordered run. Wasabi's primary
/// sidebar, however, requests active chats only. When many recently-active
/// chats are archived, that generic index has to scan and discard them before
/// it can satisfy `LIMIT`.
///
/// This partial index exactly matches the large unpinned run of
/// `ChatStore::chats_page(false, ..)`, so active keyset pages can seek and stop
/// at the page limit without duplicating the upstream query implementation.
const ACTIVE_CHAT_ORDER_INDEX: &str = "CREATE INDEX IF NOT EXISTS wasabi_chats_active_order
    ON chats (device_id, last_message_ts DESC, jid DESC)
    WHERE archived = 0 AND pinned_at IS NULL";

const ARCHIVED_CHAT_ORDER_INDEX: &str = "CREATE INDEX IF NOT EXISTS wasabi_chats_archived_order
    ON chats (device_id, last_message_ts DESC, jid DESC)
    WHERE archived = 1 AND pinned_at IS NULL";

const ARCHIVED_CHAT_PINNED_INDEX: &str = "CREATE INDEX IF NOT EXISTS wasabi_chats_archived_pinned
    ON chats (device_id, pinned_at DESC, last_message_ts DESC, jid DESC)
    WHERE archived = 1 AND pinned_at IS NOT NULL";

const CHAT_LIST_COLUMNS: &str = "jid, name, last_message_ts, last_message_preview,
    unread_count, pinned_at, muted_until";

#[derive(QueryableByName)]
pub(crate) struct ChatListRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) jid: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub(crate) name: Option<String>,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub(crate) last_message_ts: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub(crate) last_message_preview: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub(crate) unread_count: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    pub(crate) pinned_at: Option<i64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    pub(crate) muted_until: Option<i64>,
}

pub(crate) async fn ensure(shared: SharedSqlite) -> Result<(), StoreError> {
    shared
        .run(|connection| {
            for statement in [
                ACTIVE_CHAT_ORDER_INDEX,
                ARCHIVED_CHAT_ORDER_INDEX,
                ARCHIVED_CHAT_PINNED_INDEX,
            ] {
                diesel::sql_query(statement)
                    .execute(connection)
                    .map_err(|error| StoreError::Database(Box::new(error)))?;
            }
            Ok(())
        })
        .await
}

/// Read one archived-only keyset page in the same two-run order as the
/// upstream chat list: pinned first, then unpinned by activity. Active rows are
/// excluded by the partial indexes instead of being scanned and discarded.
pub(crate) async fn archived_page(
    shared: SharedSqlite,
    device_id: i32,
    after: Option<domain::ChatPageCursor>,
    limit: usize,
) -> Result<Vec<ChatListRow>, StoreError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    shared
        .read(move |connection| {
            let mut rows = Vec::new();
            let start_in_activity_run = after
                .as_ref()
                .is_some_and(|cursor| cursor.pinned_at_ms.is_none());

            if !start_in_activity_run {
                let remaining = limit - rows.len() as i64;
                if remaining > 0 {
                    let pinned = match after.as_ref() {
                        Some(cursor) => {
                            let pinned_at = cursor
                                .pinned_at_ms
                                .expect("pinned cursor while reading pinned run");
                            diesel::sql_query(format!(
                                "SELECT {CHAT_LIST_COLUMNS} FROM chats
                                 WHERE device_id = ? AND archived = 1 AND pinned_at IS NOT NULL
                                   AND (pinned_at, last_message_ts, jid) < (?, ?, ?)
                                 ORDER BY pinned_at DESC, last_message_ts DESC, jid DESC LIMIT ?"
                            ))
                            .bind::<diesel::sql_types::Integer, _>(device_id)
                            .bind::<diesel::sql_types::BigInt, _>(pinned_at)
                            .bind::<diesel::sql_types::BigInt, _>(cursor.last_activity_ms)
                            .bind::<diesel::sql_types::Text, _>(cursor.chat.as_str())
                            .bind::<diesel::sql_types::BigInt, _>(remaining)
                            .load::<ChatListRow>(connection)
                        }
                        None => diesel::sql_query(format!(
                            "SELECT {CHAT_LIST_COLUMNS} FROM chats
                             WHERE device_id = ? AND archived = 1 AND pinned_at IS NOT NULL
                             ORDER BY pinned_at DESC, last_message_ts DESC, jid DESC LIMIT ?"
                        ))
                        .bind::<diesel::sql_types::Integer, _>(device_id)
                        .bind::<diesel::sql_types::BigInt, _>(remaining)
                        .load::<ChatListRow>(connection),
                    }
                    .map_err(|error| StoreError::Database(Box::new(error)))?;
                    rows.extend(pinned);
                }
            }

            let remaining = limit - rows.len() as i64;
            if remaining > 0 {
                let activity = if start_in_activity_run {
                    let cursor = after.as_ref().expect("activity cursor");
                    diesel::sql_query(format!(
                        "SELECT {CHAT_LIST_COLUMNS} FROM chats
                         WHERE device_id = ? AND archived = 1 AND pinned_at IS NULL
                           AND (last_message_ts, jid) < (?, ?)
                         ORDER BY last_message_ts DESC, jid DESC LIMIT ?"
                    ))
                    .bind::<diesel::sql_types::Integer, _>(device_id)
                    .bind::<diesel::sql_types::BigInt, _>(cursor.last_activity_ms)
                    .bind::<diesel::sql_types::Text, _>(cursor.chat.as_str())
                    .bind::<diesel::sql_types::BigInt, _>(remaining)
                    .load::<ChatListRow>(connection)
                } else {
                    diesel::sql_query(format!(
                        "SELECT {CHAT_LIST_COLUMNS} FROM chats
                         WHERE device_id = ? AND archived = 1 AND pinned_at IS NULL
                         ORDER BY last_message_ts DESC, jid DESC LIMIT ?"
                    ))
                    .bind::<diesel::sql_types::Integer, _>(device_id)
                    .bind::<diesel::sql_types::BigInt, _>(remaining)
                    .load::<ChatListRow>(connection)
                }
                .map_err(|error| StoreError::Database(Box::new(error)))?;
                rows.extend(activity);
            }

            Ok(rows)
        })
        .await
}

//! Per-account storage: one shared SQLite database hosting both the protocol
//! store and the chat materialization store.
//!
//! This facade is the ONLY storage surface the rest of wasabi sees. ChatStore
//! types are mapped into domain projections at the edge; GPUI never touches
//! these structs directly.

use std::sync::Arc;

use diesel::prelude::*;
use tokio::sync::broadcast;

use wasabi_domain as domain;
use whatsapp_rust::wacore::proto_helpers::MessageExt;
use whatsapp_rust::wacore_binary::JidExt as _;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust_chat_store::{
    ChatStore,
    types::{ChatCursor, MessageCursor, StoreChange as UpstreamStoreChange},
};
use whatsapp_rust_sqlite_storage::{SqliteStore, SqliteStoreConfig, Synchronous};

use crate::config::StoreTuning;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("sqlite open: {0}")]
    Sqlite(String),
    #[error("chat store: {0}")]
    ChatStore(#[from] whatsapp_rust_chat_store::ChatStoreError),
    #[error("wasabi schema: {0}")]
    WasabiSchema(String),
}

/// Repository-owned durable invalidation. Upstream JIDs and store types do
/// not cross this facade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreChange {
    Chats,
    Messages { chat: String },
    Contacts,
}

pub struct StoreChangeFeed {
    inner: broadcast::Receiver<UpstreamStoreChange>,
}

impl StoreChangeFeed {
    pub async fn recv(&mut self) -> Result<StoreChange, broadcast::error::RecvError> {
        self.inner.recv().await.map(|change| match change {
            UpstreamStoreChange::Chats => StoreChange::Chats,
            UpstreamStoreChange::Contacts => StoreChange::Contacts,
            UpstreamStoreChange::Messages { chat } => StoreChange::Messages {
                chat: chat.to_string(),
            },
        })
    }
}

pub struct AccountStore {
    /// Protocol/device store. Kept alive for the session; its pool backs
    /// everything via `shared()`.
    sqlite: Arc<SqliteStore>,
    chats: Arc<ChatStore>,
}

impl AccountStore {
    /// Open (running pending migrations) the account database at `db_path`.
    pub async fn open(db_path: &std::path::Path, tuning: &StoreTuning) -> Result<Self, OpenError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| OpenError::Sqlite(format!("create dirs: {e}")))?;
        }
        let url = format!("sqlite://{}", db_path.display());
        let config = SqliteStoreConfig {
            read_pool_size: tuning.read_pool_size,
            synchronous: if tuning.synchronous_full {
                Synchronous::Full
            } else {
                Synchronous::Normal
            },
            cache_size_kib: tuning.cache_size_kib,
            busy_timeout: std::time::Duration::from_secs(tuning.busy_timeout_secs),
            ..SqliteStoreConfig::default()
        };
        let sqlite = Arc::new(
            SqliteStore::with_config(&url, config)
                .await
                .map_err(|e| OpenError::Sqlite(e.to_string()))?,
        );
        let chats = ChatStore::new(&sqlite).await?;
        crate::wasabi_schema::migrate(sqlite.shared())
            .await
            .map_err(|error| OpenError::WasabiSchema(error.to_string()))?;
        crate::chat_indexes::ensure(sqlite.shared())
            .await
            .map_err(|error| OpenError::WasabiSchema(error.to_string()))?;
        Ok(Self { sqlite, chats })
    }

    pub fn chats(&self) -> &Arc<ChatStore> {
        &self.chats
    }

    /// Direct access to the shared connection plumbing (one pool, one
    /// serialized write path). Exposed for supervisor-level operations that
    /// must share the account database's writer permit (e.g. maintenance,
    /// benchmarks) — never for bypassing the store APIs.
    pub fn shared_db(&self) -> whatsapp_rust_sqlite_storage::SharedSqlite {
        self.sqlite.shared()
    }

    /// The underlying protocol/device store, for assembling a Bot backend on
    /// top of this account's database.
    pub fn sqlite(&self) -> &Arc<SqliteStore> {
        &self.sqlite
    }

    pub fn device_id(&self) -> i32 {
        self.sqlite.device_id()
    }

    /// Subscribe to durable-change invalidations (bounded broadcast,
    /// capacity 256; lag ⇒ re-query —.
    pub fn subscribe_changes(&self) -> StoreChangeFeed {
        StoreChangeFeed {
            inner: self.chats.subscribe(),
        }
    }

    /// Barrier: every write enqueued before this call is committed when this
    /// resolves `Ok`. Conservative on failure (false-failure possible,
    /// false-success never).
    pub async fn flush(&self) -> Result<(), whatsapp_rust_chat_store::ChatStoreError> {
        self.chats.flush().await
    }

    /// Persist a server-acknowledged empty group so it remains discoverable
    /// before its first message. The timestamp comes from server metadata;
    /// no preview or synthetic system message is invented.
    pub async fn record_created_group(
        &self,
        chat: domain::ChatId,
        subject: String,
        created_at_ms: i64,
    ) -> Result<(), domain::ServiceError> {
        let jid = parse_jid(chat.as_str())?;
        if !jid.is_group() {
            return Err(domain::ServiceError::new(
                domain::ErrorKind::InvalidRequest,
                "created conversation is not a group",
            ));
        }
        let subject = subject.trim().to_string();
        if subject.is_empty() {
            return Err(domain::ServiceError::new(
                domain::ErrorKind::InvalidRequest,
                "created group has no subject",
            ));
        }
        let device_id = self.device_id();
        let chat = jid.to_string();
        self.sqlite
            .shared()
            .run(move |connection| {
                diesel::sql_query(
                    "INSERT INTO chats (device_id, jid, name, last_message_ts) \
                     VALUES (?, ?, ?, ?) \
                     ON CONFLICT(device_id, jid) DO UPDATE SET \
                       name = excluded.name, \
                       last_message_ts = MAX(chats.last_message_ts, excluded.last_message_ts)",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::Text, _>(chat)
                .bind::<diesel::sql_types::Text, _>(subject)
                .bind::<diesel::sql_types::BigInt, _>(created_at_ms.max(0))
                .execute(connection)
                .map(|_| ())
                .map_err(|error| wacore::store::error::StoreError::Database(Box::new(error)))
            })
            .await
            .map_err(|error| {
                domain::ServiceError::new(domain::ErrorKind::Database, error.to_string())
            })
    }

    /// Replace the last-known group metadata and participant snapshot in one
    /// transaction. A partial participant refresh is never exposed.
    pub async fn save_group_details(
        &self,
        details: domain::GroupDetails,
        fetched_at_ms: i64,
    ) -> Result<(), domain::ServiceError> {
        crate::group_cache::save(
            self.sqlite.shared(),
            self.device_id(),
            details,
            fetched_at_ms,
        )
        .await
    }

    /// Read the last truthful group snapshot for disconnected rendering.
    pub async fn cached_group_details(
        &self,
        chat: &str,
    ) -> Result<Option<domain::GroupDetails>, domain::ServiceError> {
        crate::group_cache::load(self.sqlite.shared(), self.device_id(), chat.to_string()).await
    }

    /// Forget cached group identity and participant data after an acknowledged
    /// leave operation. Repeating this operation is safe.
    pub async fn remove_cached_group_details(
        &self,
        chat: &str,
    ) -> Result<(), domain::ServiceError> {
        crate::group_cache::remove(self.sqlite.shared(), self.device_id(), chat.to_string()).await
    }

    // ---- Query facade -------------------------------------------------

    /// One keyset page of the chat list.
    pub async fn chat_page(
        &self,
        scope: domain::ChatScope,
        after: Option<domain::page::ChatPageCursor>,
        limit: usize,
    ) -> Result<domain::ChatPage, domain::ServiceError> {
        if limit == 0 {
            return Ok(domain::ChatPage {
                rows: Vec::new(),
                next_after: None,
            });
        }

        match scope {
            domain::ChatScope::Active => self.unfiltered_chat_page(false, after, limit).await,
            domain::ChatScope::All => self.unfiltered_chat_page(true, after, limit).await,
            domain::ChatScope::Archived => self.archived_chat_page(after, limit).await,
        }
    }

    async fn unfiltered_chat_page(
        &self,
        include_archived: bool,
        after: Option<domain::ChatPageCursor>,
        limit: usize,
    ) -> Result<domain::ChatPage, domain::ServiceError> {
        let upstream_after = after.map(domain_cursor_to_upstream);
        let fetch = limit.saturating_add(1);
        let mut rows = self
            .chats
            .chats_page(include_archived, upstream_after, fetch as i64)
            .await
            .map_err(database_error)?
            .into_iter()
            .map(chat_entry_to_summary)
            .collect::<Vec<_>>();
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        self.hydrate_chat_preferences(&mut rows).await?;
        let next_after =
            has_more.then(|| chat_summary_cursor(rows.last().expect("non-empty page")));
        Ok(domain::ChatPage { rows, next_after })
    }

    /// Archived-only keyset page, preserving the upstream pinned-first then
    /// activity ordering without scanning unrelated active chats.
    async fn archived_chat_page(
        &self,
        after: Option<domain::ChatPageCursor>,
        limit: usize,
    ) -> Result<domain::ChatPage, domain::ServiceError> {
        let fetch = limit.saturating_add(1);
        let mut rows = crate::chat_indexes::archived_page(
            self.shared_db(),
            self.device_id(),
            after,
            fetch,
        )
        .await
        .map_err(|error| {
            domain::ServiceError::new(domain::ErrorKind::Database, error.to_string())
        })?
        .into_iter()
        .map(archived_chat_row_to_summary)
        .collect::<Vec<_>>();
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        self.hydrate_chat_preferences(&mut rows).await?;
        let next_after =
            has_more.then(|| chat_summary_cursor(rows.last().expect("non-empty page")));
        Ok(domain::ChatPage {
            rows,
            next_after,
        })
    }

    /// One keyset page of messages for a chat, newest→oldest.
    pub async fn message_page(
        &self,
        chat: &str,
        before: Option<domain::PageCursor>,
        limit: usize,
    ) -> Result<domain::MessagePage, domain::ServiceError> {
        let jid = parse_jid(chat)?;
        let before = before.map(|c| MessageCursor {
            timestamp_ms: c.timestamp_ms,
            seq: c.seq.0,
        });
        let rows = self
            .chats
            .messages(&jid, before, limit as i64)
            .await
            .map_err(|e| domain::ServiceError::new(domain::ErrorKind::Database, e.to_string()))?;
        let next_before = rows.last().map(|m| domain::PageCursor {
            timestamp_ms: m.timestamp.timestamp_millis(),
            seq: domain::LocalCursor(m.seq),
        });
        let mut out = rows
            .into_iter()
            .map(stored_to_row)
            .collect::<Result<Vec<_>, domain::ServiceError>>()?;
        self.attach_reaction_summaries(&mut out).await?;
        Ok(domain::MessagePage {
            rows: out,
            next_before,
        })
    }

    /// A bounded window around an exact durable message identity. The target
    /// is resolved through ChatStore first so PN/LID aliases land on the same
    /// canonical conversation before the two indexed neighbor reads run.
    pub async fn message_context(
        &self,
        chat: &str,
        anchor: domain::MessageId,
        before: usize,
        after: usize,
    ) -> Result<domain::MessageContext, domain::ServiceError> {
        let requested_chat = parse_jid(chat)?;
        let target = self
            .chats
            .message(&requested_chat, anchor.as_str())
            .await
            .map_err(database_error)?
            .ok_or_else(|| {
                domain::ServiceError::new(
                    domain::ErrorKind::InvalidRequest,
                    "message anchor no longer exists",
                )
            })?;
        let device_id = self.device_id();
        let actual_chat = target.chat_jid.to_string();
        let timestamp_ms = target.timestamp.timestamp_millis();
        let sequence = target.seq;
        let before_limit = i64::try_from(before.saturating_add(1)).unwrap_or(i64::MAX);
        let after_limit = i64::try_from(after.saturating_add(1)).unwrap_or(i64::MAX);
        let query_chat = actual_chat.clone();

        let (mut older, mut newer) = self
            .shared_db()
            .read(move |connection| {
                let older = diesel::sql_query(
                    "SELECT msg_id, sender_jid, from_me, timestamp_ms, kind, text_content, proto, \
                            status, starred, edited_at_ms, revoked, rowid \
                     FROM messages \
                     WHERE device_id = ? AND chat_jid = ? \
                       AND (timestamp_ms < ? OR (timestamp_ms = ? AND rowid < ?)) \
                     ORDER BY timestamp_ms DESC, rowid DESC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::Text, _>(query_chat.clone())
                .bind::<diesel::sql_types::BigInt, _>(timestamp_ms)
                .bind::<diesel::sql_types::BigInt, _>(timestamp_ms)
                .bind::<diesel::sql_types::BigInt, _>(sequence)
                .bind::<diesel::sql_types::BigInt, _>(before_limit)
                .load::<MessageContextRow>(connection)
                .map_err(context_database_error)?;
                let newer = diesel::sql_query(
                    "SELECT msg_id, sender_jid, from_me, timestamp_ms, kind, text_content, proto, \
                            status, starred, edited_at_ms, revoked, rowid \
                     FROM messages \
                     WHERE device_id = ? AND chat_jid = ? \
                       AND (timestamp_ms > ? OR (timestamp_ms = ? AND rowid > ?)) \
                     ORDER BY timestamp_ms ASC, rowid ASC LIMIT ?",
                )
                .bind::<diesel::sql_types::Integer, _>(device_id)
                .bind::<diesel::sql_types::Text, _>(query_chat)
                .bind::<diesel::sql_types::BigInt, _>(timestamp_ms)
                .bind::<diesel::sql_types::BigInt, _>(timestamp_ms)
                .bind::<diesel::sql_types::BigInt, _>(sequence)
                .bind::<diesel::sql_types::BigInt, _>(after_limit)
                .load::<MessageContextRow>(connection)
                .map_err(context_database_error)?;
                Ok((older, newer))
            })
            .await
            .map_err(|error| {
                domain::ServiceError::new(domain::ErrorKind::Database, error.to_string())
            })?;

        let has_more_older = older.len() > before;
        let has_more_newer = newer.len() > after;
        older.truncate(before);
        newer.truncate(after);

        // Context uses the established newest→oldest boundary order: reverse
        // the nearest-first newer side, then target, then older DESC rows.
        newer.reverse();
        let mut rows = newer
            .into_iter()
            .map(|row| context_row_to_domain(&actual_chat, row))
            .collect::<Vec<_>>();
        rows.push(stored_to_row(target)?);
        rows.extend(
            older
                .into_iter()
                .map(|row| context_row_to_domain(&actual_chat, row)),
        );
        self.attach_reaction_summaries(&mut rows).await?;
        Ok(domain::MessageContext {
            rows,
            anchor,
            has_more_older,
            has_more_newer,
        })
    }

    async fn attach_reaction_summaries(
        &self,
        rows: &mut [domain::MessageRow],
    ) -> Result<(), domain::ServiceError> {
        if rows.is_empty() {
            return Ok(());
        }
        let device = self
            .sqlite
            .load_device_data_for_device(self.device_id())
            .await
            .map_err(|error| {
                domain::ServiceError::new(domain::ErrorKind::Database, error.to_string())
            })?;
        let own_jids = device
            .into_iter()
            .flat_map(|device| [device.pn, device.lid])
            .flatten()
            .map(|jid| jid.to_non_ad().to_string())
            .collect::<std::collections::HashSet<_>>();
        let reactions = futures::future::try_join_all(rows.iter().map(|row| {
            let chat = row.chat.as_str().parse::<whatsapp_rust::Jid>();
            let message = row.id.as_str().to_string();
            async move {
                let chat = chat.map_err(|error| {
                    domain::ServiceError::new(domain::ErrorKind::InvalidRequest, error.to_string())
                })?;
                self.chats
                    .reactions(&chat, &message)
                    .await
                    .map_err(database_error)
            }
        }))
        .await?;
        for (row, entries) in rows.iter_mut().zip(reactions) {
            row.reactions = aggregate_reactions(entries, &own_jids);
        }
        Ok(())
    }

    /// Latest committed message plus chat-level notification policy inputs.
    /// No protocol/store type crosses the repository facade.
    pub async fn notification_candidate(
        &self,
        chat: &str,
    ) -> Result<Option<domain::NotificationCandidate>, domain::ServiceError> {
        let jid = parse_jid(chat)?;
        let Some(entry) = self.chats.chat(&jid).await.map_err(database_error)? else {
            return Ok(None);
        };
        let Some(message) = self
            .chats
            .messages(&jid, None, 1)
            .await
            .map_err(database_error)?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let row = stored_to_row(message)?;
        let now = chrono::Utc::now().timestamp_millis();
        let muted = entry
            .muted_until
            .is_some_and(|until| until.timestamp_millis() == 0 || until.timestamp_millis() > now);
        let (preview, eligible) = notification_preview(&row.kind);
        Ok(Some(domain::NotificationCandidate {
            chat: row.chat.clone(),
            message: row.id.clone(),
            title: entry.name.unwrap_or_else(|| jid.user.to_string()),
            preview,
            timestamp_ms: row.timestamp_ms,
            outgoing: row.direction == domain::MessageDirection::Outgoing,
            muted,
            eligible,
        }))
    }

    /// Cached identity fields for a direct contact. Privacy-controlled About
    /// and profile-photo data remain `None` until their dedicated cache is
    /// populated; the projection never invents values for them.
    pub async fn direct_contact_details(
        &self,
        jid: &str,
    ) -> Result<domain::DirectContactDetails, domain::ServiceError> {
        let jid = parse_jid(jid)?;
        let contact = self.chats.contact(&jid).await.map_err(database_error)?;
        let display_name = contact
            .as_ref()
            .and_then(|contact| contact.display_name())
            .map(str::to_string)
            .unwrap_or_else(|| jid.user.to_string());
        let phone_number = jid.is_pn().then(|| jid.user.to_string());
        let cached =
            crate::contacts::load_metadata(self.shared_db(), self.device_id(), jid.to_string())
                .await?;
        let cached_name = cached
            .as_ref()
            .and_then(|cached| cached.display_name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        Ok(domain::DirectContactDetails {
            jid: jid.to_string(),
            display_name: cached_name.unwrap_or(display_name),
            phone_number,
            about: cached.as_ref().and_then(|cached| cached.about.clone()),
            avatar: cached
                .and_then(|cached| cached.avatar_ref)
                .map(domain::AvatarRef),
        })
    }

    pub async fn save_direct_contact_metadata(
        &self,
        details: &domain::DirectContactDetails,
    ) -> Result<(), domain::ServiceError> {
        crate::contacts::save_metadata(
            self.shared_db(),
            self.device_id(),
            details.jid.clone(),
            Some(details.display_name.clone()),
            details.about.clone(),
            details.avatar.as_ref().map(|avatar| avatar.0.clone()),
        )
        .await
    }

    pub async fn contact_page(
        &self,
        query: String,
        after: Option<domain::ContactPageCursor>,
        limit: usize,
    ) -> Result<domain::ContactPage, domain::ServiceError> {
        crate::contacts::page(self.shared_db(), self.device_id(), query, after, limit).await
    }

    pub async fn chat_preference(
        &self,
        chat: domain::ChatId,
    ) -> Result<crate::preferences::ChatPreference, domain::ServiceError> {
        crate::preferences::load(self.shared_db(), self.device_id(), chat).await
    }

    pub async fn set_favorite(
        &self,
        chat: domain::ChatId,
        favorite: bool,
    ) -> Result<(), domain::ServiceError> {
        crate::preferences::set_favorite(self.shared_db(), self.device_id(), chat, favorite).await
    }

    pub async fn save_draft(
        &self,
        chat: domain::ChatId,
        draft: Option<domain::Draft>,
    ) -> Result<(), domain::ServiceError> {
        crate::preferences::save_draft(self.shared_db(), self.device_id(), chat, draft).await
    }

    pub async fn save_transfer_job(
        &self,
        job: domain::TransferJob,
    ) -> Result<(), domain::ServiceError> {
        crate::transfers::save(self.shared_db(), self.device_id(), job).await
    }

    pub async fn transfer_jobs(
        &self,
        include_terminal: bool,
    ) -> Result<Vec<domain::TransferJob>, domain::ServiceError> {
        crate::transfers::load(self.shared_db(), self.device_id(), include_terminal).await
    }

    pub async fn transfer_job(
        &self,
        transfer: domain::TransferId,
    ) -> Result<Option<domain::TransferJob>, domain::ServiceError> {
        crate::transfers::load_one(self.shared_db(), self.device_id(), transfer).await
    }

    pub async fn update_transfer_payload(
        &self,
        transfer: domain::TransferId,
        payload: domain::TransferPayload,
    ) -> Result<bool, domain::ServiceError> {
        crate::transfers::update_payload(self.shared_db(), self.device_id(), transfer, payload)
            .await
    }

    pub async fn update_transfer_progress(
        &self,
        transfer: domain::TransferId,
        bytes_done: u64,
        bytes_total: Option<u64>,
    ) -> Result<bool, domain::ServiceError> {
        crate::transfers::update_progress(
            self.shared_db(),
            self.device_id(),
            transfer,
            bytes_done,
            bytes_total,
        )
        .await
    }

    pub async fn set_transfer_state(
        &self,
        transfer: domain::TransferId,
        state: domain::TransferState,
        error_kind: Option<domain::ErrorKind>,
    ) -> Result<bool, domain::ServiceError> {
        crate::transfers::set_state(
            self.shared_db(),
            self.device_id(),
            transfer,
            state,
            error_kind,
        )
        .await
    }

    pub async fn remove_terminal_transfer(
        &self,
        transfer: domain::TransferId,
    ) -> Result<bool, domain::ServiceError> {
        crate::transfers::remove_terminal(self.shared_db(), self.device_id(), transfer).await
    }

    async fn hydrate_chat_preferences(
        &self,
        chats: &mut [domain::ChatSummary],
    ) -> Result<(), domain::ServiceError> {
        let ids = chats
            .iter()
            .map(|chat| chat.id.as_str().to_string())
            .collect();
        let preferences =
            crate::preferences::load_for_chats(self.shared_db(), self.device_id(), ids).await?;
        for chat in chats {
            let Some(preference) = preferences.get(chat.id.as_str()) else {
                continue;
            };
            chat.favorite = preference.favorite;
            chat.draft = preference.draft.clone();
            chat.draft_preview = preference
                .draft
                .as_ref()
                .map(|draft| draft.body.trim())
                .filter(|body| !body.is_empty())
                .map(str::to_string);
        }
        Ok(())
    }
}

fn domain_cursor_to_upstream(cursor: domain::ChatPageCursor) -> ChatCursor {
    ChatCursor {
        pinned_at_ms: cursor.pinned_at_ms,
        last_message_ts: cursor.last_activity_ms,
        jid: cursor.chat.as_str().to_string(),
    }
}

fn chat_summary_cursor(chat: &domain::ChatSummary) -> domain::ChatPageCursor {
    domain::ChatPageCursor {
        pinned_at_ms: chat.pinned_at_ms,
        last_activity_ms: chat.last_activity_ms,
        chat: chat.id.clone(),
    }
}

fn database_error(error: whatsapp_rust_chat_store::ChatStoreError) -> domain::ServiceError {
    domain::ServiceError::new(domain::ErrorKind::Database, error.to_string())
}

impl Drop for AccountStore {
    fn drop(&mut self) {
        // Writer task stops when ChatStore drops; pools close with the last
        // connection. Nothing to leak.
    }
}

fn parse_jid(s: &str) -> Result<whatsapp_rust::Jid, domain::ServiceError> {
    s.parse::<whatsapp_rust::Jid>().map_err(|e| {
        domain::ServiceError::new(domain::ErrorKind::InvalidRequest, format!("bad jid: {e}"))
    })
}

fn chat_entry_to_summary(e: whatsapp_rust_chat_store::types::ChatEntry) -> domain::ChatSummary {
    let jid = e.jid.to_string();
    domain::ChatSummary {
        kind: chat_kind(&jid),
        id: domain::ChatId::new(jid),
        display_name: e.name,
        last_activity_ms: e.last_message_at.map_or(0, |t| t.timestamp_millis()),
        last_message_preview: e.last_message_preview,
        unread_count: e.unread_count as i64,
        pinned_at_ms: e.pinned_at.map(|t| t.timestamp_millis()),
        muted_until_ms: e.muted_until.map(|t| t.timestamp_millis()),
        archived: e.archived,
        favorite: false,
        draft_preview: None,
        draft: None,
    }
}

fn archived_chat_row_to_summary(row: crate::chat_indexes::ChatListRow) -> domain::ChatSummary {
    let kind = chat_kind(&row.jid);
    domain::ChatSummary {
        id: domain::ChatId::new(row.jid),
        kind,
        display_name: row.name,
        last_activity_ms: row.last_message_ts,
        last_message_preview: row.last_message_preview,
        unread_count: row.unread_count as i64,
        pinned_at_ms: row.pinned_at,
        muted_until_ms: row.muted_until,
        archived: true,
        favorite: false,
        draft_preview: None,
        draft: None,
    }
}

fn chat_kind(jid: &str) -> domain::ChatKind {
    if jid.ends_with("@g.us") {
        domain::ChatKind::Group
    } else if jid.ends_with("@newsletter") {
        domain::ChatKind::Newsletter
    } else if jid.ends_with("@broadcast") || jid == "status@broadcast" {
        domain::ChatKind::System
    } else {
        domain::ChatKind::Direct
    }
}

#[cfg(test)]
mod projection_tests {
    use std::collections::HashSet;

    use super::{aggregate_reactions, chat_kind, map_kind_fields, map_quoted_message};
    use wasabi_domain::{ChatKind, MessageKind, UnavailableMessageReason};
    use whatsapp_rust::chrono::Utc;
    use whatsapp_rust::wacore::proto_helpers::{MessageBuilderExt, build_quote_context};
    use whatsapp_rust::waproto::whatsapp as wa;

    #[test]
    fn chat_kind_is_derived_from_stable_jid_server() {
        assert_eq!(chat_kind("123@s.whatsapp.net"), ChatKind::Direct);
        assert_eq!(chat_kind("123@g.us"), ChatKind::Group);
        assert_eq!(chat_kind("123@newsletter"), ChatKind::Newsletter);
        assert_eq!(chat_kind("status@broadcast"), ChatKind::System);
    }

    #[test]
    fn unavailable_kinds_keep_distinct_recovery_reasons() {
        let cases = [
            (
                "undecryptable",
                UnavailableMessageReason::WaitingForDecryption,
            ),
            ("view_once", UnavailableMessageReason::ViewOnceOnPhone),
            ("hosted", UnavailableMessageReason::HostedContent),
            ("bot", UnavailableMessageReason::BotContent),
        ];
        for (label, expected) in cases {
            assert_eq!(
                map_kind_fields("M", label, None, None),
                MessageKind::Unavailable { reason: expected }
            );
        }
        assert_eq!(
            map_kind_fields("M", "unknown", None, None),
            MessageKind::Unknown
        );
    }

    #[test]
    fn quote_projection_is_bounded_and_keeps_the_original_identity() {
        let original = wa::Message::text(format!("first line\n{}", "界".repeat(200)));
        let reply = wa::Message::text_with_context(
            "reply",
            build_quote_context("ORIGINAL-ID", "15550000000@s.whatsapp.net", &original),
        );
        let quoted = map_quoted_message(&reply).expect("quoted projection");

        assert_eq!(quoted.id.as_str(), "ORIGINAL-ID");
        assert_eq!(quoted.sender.as_deref(), Some("15550000000@s.whatsapp.net"));
        assert!(!quoted.preview.contains('\n'));
        assert!(quoted.preview.ends_with('…'));
        assert!(quoted.preview.chars().count() <= 161);
    }

    #[test]
    fn reactions_aggregate_in_first_seen_order_and_mark_own_choice() {
        let entry = |sender: &str, emoji: &str| whatsapp_rust_chat_store::ReactionEntry {
            sender_jid: sender.parse().unwrap(),
            emoji: emoji.to_string(),
            timestamp: Utc::now(),
        };
        let own = HashSet::from(["15550000001@s.whatsapp.net".to_string()]);
        let summaries = aggregate_reactions(
            vec![
                entry("15550000000@s.whatsapp.net", "👍"),
                entry("15550000001@s.whatsapp.net", "❤️"),
                entry("15550000002@s.whatsapp.net", "👍"),
            ],
            &own,
        );

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].emoji, "👍");
        assert_eq!(summaries[0].count, 2);
        assert!(!summaries[0].reacted_by_me);
        assert_eq!(summaries[1].emoji, "❤️");
        assert!(summaries[1].reacted_by_me);
    }
}

fn aggregate_reactions(
    entries: Vec<whatsapp_rust_chat_store::ReactionEntry>,
    own_jids: &std::collections::HashSet<String>,
) -> Vec<domain::ReactionSummary> {
    let mut summaries = Vec::<domain::ReactionSummary>::new();
    for entry in entries {
        if entry.emoji.is_empty() {
            continue;
        }
        let own = own_jids.contains(&entry.sender_jid.to_non_ad().to_string());
        if let Some(summary) = summaries
            .iter_mut()
            .find(|summary| summary.emoji == entry.emoji)
        {
            summary.count = summary.count.saturating_add(1);
            summary.reacted_by_me |= own;
        } else {
            summaries.push(domain::ReactionSummary {
                emoji: entry.emoji,
                count: 1,
                reacted_by_me: own,
            });
        }
    }
    summaries
}

pub(crate) fn stored_to_row(
    m: whatsapp_rust_chat_store::types::StoredMessage,
) -> Result<domain::MessageRow, domain::ServiceError> {
    use whatsapp_rust_chat_store::types::MessageStatus as UpStatus;
    let status = match m.status {
        UpStatus::Error => domain::MessageStatus::Failed,
        UpStatus::Pending => domain::MessageStatus::Pending,
        UpStatus::ServerAck => domain::MessageStatus::ServerAck,
        UpStatus::Delivered | UpStatus::Played => domain::MessageStatus::Delivered,
        UpStatus::Read => domain::MessageStatus::Read,
    };
    let kind = map_kind(&m);
    let quoted = m.message.as_deref().and_then(map_quoted_message);
    Ok(domain::MessageRow {
        id: domain::MessageId::new(m.id),
        chat: domain::ChatId::new(m.chat_jid.to_string()),
        direction: if m.from_me {
            domain::MessageDirection::Outgoing
        } else {
            domain::MessageDirection::Incoming
        },
        sender: domain::SenderJid {
            bare: m.sender_jid.to_string(),
            push_name: None,
        },
        timestamp_ms: m.timestamp.timestamp_millis(),
        seq: domain::LocalCursor(m.seq),
        kind,
        quoted,
        reactions: Vec::new(),
        status,
        edited_at_ms: m.edited_at.map(|t| t.timestamp_millis()),
        revoked: m.revoked,
        starred: m.starred,
    })
}

#[derive(QueryableByName)]
struct MessageContextRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    msg_id: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    sender_jid: String,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    from_me: bool,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    timestamp_ms: i64,
    #[diesel(sql_type = diesel::sql_types::Text)]
    kind: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    text_content: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Binary>)]
    proto: Option<Vec<u8>>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    status: i32,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    starred: bool,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    edited_at_ms: Option<i64>,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    revoked: bool,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    rowid: i64,
}

fn context_row_to_domain(chat: &str, row: MessageContextRow) -> domain::MessageRow {
    let status = match row.status {
        0 => domain::MessageStatus::Failed,
        2 => domain::MessageStatus::ServerAck,
        3 | 5 => domain::MessageStatus::Delivered,
        4 => domain::MessageStatus::Read,
        _ => domain::MessageStatus::Pending,
    };
    let decoded = row
        .proto
        .as_deref()
        .and_then(|bytes| whatsapp_rust::waproto::codec::message_decode(bytes).ok());
    let kind = map_kind_fields(&row.msg_id, &row.kind, row.text_content, decoded.as_ref());
    let quoted = decoded.as_ref().and_then(map_quoted_message);
    domain::MessageRow {
        id: domain::MessageId::new(row.msg_id),
        chat: domain::ChatId::new(chat),
        direction: if row.from_me {
            domain::MessageDirection::Outgoing
        } else {
            domain::MessageDirection::Incoming
        },
        sender: domain::SenderJid {
            bare: row.sender_jid,
            push_name: None,
        },
        timestamp_ms: row.timestamp_ms,
        seq: domain::LocalCursor(row.rowid),
        kind,
        quoted,
        reactions: Vec::new(),
        status,
        edited_at_ms: row.edited_at_ms,
        revoked: row.revoked,
        starred: row.starred,
    }
}

fn map_quoted_message(message: &wa::Message) -> Option<domain::QuotedMessage> {
    let context = first_context_info(message)?;
    let id = context.stanza_id.as_ref()?.trim();
    if id.is_empty() {
        return None;
    }
    let quoted = context.quoted_message.as_option();
    let preview = quoted.map_or_else(
        || "Original message unavailable".to_string(),
        quoted_preview,
    );
    Some(domain::QuotedMessage {
        id: domain::MessageId::new(id),
        sender: context
            .participant
            .clone()
            .filter(|sender| !sender.is_empty()),
        preview,
    })
}

fn first_context_info(message: &wa::Message) -> Option<&wa::ContextInfo> {
    let base = message.get_base_message();
    base.extended_text_message
        .as_option()
        .and_then(|message| message.context_info.as_option())
        .or_else(|| {
            base.image_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.video_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.ptv_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.audio_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.document_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.sticker_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
}

fn quoted_preview(message: &wa::Message) -> String {
    if let Some(text) = message.text_content().or_else(|| message.get_caption()) {
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() {
            let mut chars = normalized.chars();
            let preview = chars.by_ref().take(160).collect::<String>();
            return if chars.next().is_some() {
                format!("{preview}…")
            } else {
                preview
            };
        }
    }
    let base = message.get_base_message();
    if base.image_message.is_set() {
        "Photo".to_string()
    } else if base.video_message.is_set() || base.ptv_message.is_set() {
        "Video".to_string()
    } else if base.audio_message.is_set() {
        "Audio".to_string()
    } else if base.document_message.is_set() {
        "Document".to_string()
    } else if base.sticker_message.is_set() {
        "Sticker".to_string()
    } else {
        "Message".to_string()
    }
}

fn notification_preview(kind: &domain::MessageKind) -> (String, bool) {
    match kind {
        domain::MessageKind::Text { body } => (body.clone(), true),
        domain::MessageKind::Image { caption, .. } => {
            (caption.clone().unwrap_or_else(|| "Photo".to_string()), true)
        }
        domain::MessageKind::Video { caption, .. } => {
            (caption.clone().unwrap_or_else(|| "Video".to_string()), true)
        }
        domain::MessageKind::Audio { .. } => ("Voice message".to_string(), true),
        domain::MessageKind::Document { media } => (
            media
                .file_name
                .clone()
                .unwrap_or_else(|| "Document".to_string()),
            true,
        ),
        domain::MessageKind::Sticker { .. } => ("Sticker".to_string(), true),
        domain::MessageKind::Unavailable { reason } => (
            match reason {
                domain::UnavailableMessageReason::WaitingForDecryption => "Waiting for message",
                domain::UnavailableMessageReason::ViewOnceOnPhone => "View-once message",
                domain::UnavailableMessageReason::HostedContent => "Hosted message",
                domain::UnavailableMessageReason::BotContent => "Automated message",
            }
            .to_string(),
            true,
        ),
        domain::MessageKind::Unknown => ("Unsupported message".to_string(), true),
        domain::MessageKind::Reaction { .. } | domain::MessageKind::System { .. } => {
            (String::new(), false)
        }
    }
}

fn context_database_error(error: diesel::result::Error) -> wacore::store::error::StoreError {
    wacore::store::error::StoreError::Database(Box::new(error))
}

/// Map the stored kind + text into the UI-facing projection. Media payloads
/// stay behind handles added; nothing here carries bytes.
fn map_kind(m: &whatsapp_rust_chat_store::types::StoredMessage) -> domain::MessageKind {
    map_kind_fields(&m.id, m.kind.as_str(), m.text.clone(), m.message.as_deref())
}

fn map_kind_fields(
    message_id: &str,
    kind: &str,
    text: Option<String>,
    message: Option<&wa::Message>,
) -> domain::MessageKind {
    let base = message.map(MessageExt::get_base_message);
    match kind {
        "text" => domain::MessageKind::Text {
            body: text.unwrap_or_default(),
        },
        "image" => {
            let wire = base.and_then(|base| base.image_message.as_option());
            domain::MessageKind::Image {
                caption: wire.and_then(|media| media.caption.clone()).or(text),
                media: image_descriptor(message_id, wire),
            }
        }
        "video" | "ptv" => {
            let wire = base.and_then(|base| {
                base.video_message
                    .as_option()
                    .or_else(|| base.ptv_message.as_option())
            });
            domain::MessageKind::Video {
                caption: wire.and_then(|media| media.caption.clone()).or(text),
                video_note: kind == "ptv",
                media: video_descriptor(message_id, wire),
            }
        }
        "audio" | "ptt" => {
            let wire = base.and_then(|base| base.audio_message.as_option());
            domain::MessageKind::Audio {
                voice_note: kind == "ptt" || wire.and_then(|media| media.ptt).unwrap_or(false),
                media: audio_descriptor(message_id, wire),
            }
        }
        "document" => {
            let wire = base.and_then(|base| base.document_message.as_option());
            domain::MessageKind::Document {
                media: document_descriptor(message_id, wire, text),
            }
        }
        "sticker" => {
            let wire = base.and_then(|base| base.sticker_message.as_option());
            domain::MessageKind::Sticker {
                animated: wire.and_then(|media| media.is_animated).unwrap_or(false),
                media: sticker_descriptor(message_id, wire),
            }
        }
        "undecryptable" => domain::MessageKind::Unavailable {
            reason: domain::UnavailableMessageReason::WaitingForDecryption,
        },
        "view_once" => domain::MessageKind::Unavailable {
            reason: domain::UnavailableMessageReason::ViewOnceOnPhone,
        },
        "hosted" => domain::MessageKind::Unavailable {
            reason: domain::UnavailableMessageReason::HostedContent,
        },
        "bot" => domain::MessageKind::Unavailable {
            reason: domain::UnavailableMessageReason::BotContent,
        },
        "unknown" => domain::MessageKind::Unknown,
        _ => text.map_or(domain::MessageKind::Unknown, |text| {
            domain::MessageKind::System { text }
        }),
    }
}

fn media_availability(
    url: Option<&String>,
    direct_path: Option<&String>,
    media_key: Option<&Vec<u8>>,
    file_sha256: Option<&Vec<u8>>,
) -> domain::MediaAvailability {
    if (url.is_some() || direct_path.is_some()) && media_key.is_some() && file_sha256.is_some() {
        domain::MediaAvailability::Remote
    } else {
        domain::MediaAvailability::Unavailable
    }
}

fn media_descriptor(
    message_id: &str,
    mime_type: Option<String>,
    file_name: Option<String>,
    file_size: Option<u64>,
    duration_seconds: Option<u32>,
    dimensions: (Option<u32>, Option<u32>),
    availability: domain::MediaAvailability,
) -> domain::MediaDescriptor {
    let (width, height) = dimensions;
    domain::MediaDescriptor {
        id: domain::MediaId::new(message_id),
        mime_type,
        file_name,
        file_size,
        duration_seconds,
        width,
        height,
        availability,
    }
}

fn image_descriptor(
    message_id: &str,
    media: Option<&wa::message::ImageMessage>,
) -> domain::MediaDescriptor {
    let availability = media.map_or(domain::MediaAvailability::Unavailable, |media| {
        media_availability(
            media.url.as_ref(),
            media.direct_path.as_ref(),
            media.media_key.as_ref(),
            media.file_sha256.as_ref(),
        )
    });
    media_descriptor(
        message_id,
        media.and_then(|media| media.mimetype.clone()),
        None,
        media.and_then(|media| media.file_length),
        None,
        (
            media.and_then(|media| media.width),
            media.and_then(|media| media.height),
        ),
        availability,
    )
}

fn video_descriptor(
    message_id: &str,
    media: Option<&wa::message::VideoMessage>,
) -> domain::MediaDescriptor {
    let availability = media.map_or(domain::MediaAvailability::Unavailable, |media| {
        media_availability(
            media.url.as_ref(),
            media.direct_path.as_ref(),
            media.media_key.as_ref(),
            media.file_sha256.as_ref(),
        )
    });
    media_descriptor(
        message_id,
        media.and_then(|media| media.mimetype.clone()),
        None,
        media.and_then(|media| media.file_length),
        media.and_then(|media| media.seconds),
        (
            media.and_then(|media| media.width),
            media.and_then(|media| media.height),
        ),
        availability,
    )
}

fn audio_descriptor(
    message_id: &str,
    media: Option<&wa::message::AudioMessage>,
) -> domain::MediaDescriptor {
    let availability = media.map_or(domain::MediaAvailability::Unavailable, |media| {
        media_availability(
            media.url.as_ref(),
            media.direct_path.as_ref(),
            media.media_key.as_ref(),
            media.file_sha256.as_ref(),
        )
    });
    media_descriptor(
        message_id,
        media.and_then(|media| media.mimetype.clone()),
        None,
        media.and_then(|media| media.file_length),
        media.and_then(|media| media.seconds),
        (None, None),
        availability,
    )
}

fn document_descriptor(
    message_id: &str,
    media: Option<&wa::message::DocumentMessage>,
    fallback_name: Option<String>,
) -> domain::MediaDescriptor {
    let availability = media.map_or(domain::MediaAvailability::Unavailable, |media| {
        media_availability(
            media.url.as_ref(),
            media.direct_path.as_ref(),
            media.media_key.as_ref(),
            media.file_sha256.as_ref(),
        )
    });
    media_descriptor(
        message_id,
        media.and_then(|media| media.mimetype.clone()),
        media
            .and_then(|media| media.file_name.clone())
            .or(fallback_name),
        media.and_then(|media| media.file_length),
        None,
        (None, None),
        availability,
    )
}

fn sticker_descriptor(
    message_id: &str,
    media: Option<&wa::message::StickerMessage>,
) -> domain::MediaDescriptor {
    let availability = media.map_or(domain::MediaAvailability::Unavailable, |media| {
        media_availability(
            media.url.as_ref(),
            media.direct_path.as_ref(),
            media.media_key.as_ref(),
            media.file_sha256.as_ref(),
        )
    });
    media_descriptor(
        message_id,
        media.and_then(|media| media.mimetype.clone()),
        None,
        media.and_then(|media| media.file_length),
        None,
        (
            media.and_then(|media| media.width),
            media.and_then(|media| media.height),
        ),
        availability,
    )
}

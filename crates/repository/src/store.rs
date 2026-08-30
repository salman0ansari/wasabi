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

    /// Cached groups whose participant snapshot includes this direct contact.
    /// PN and known LID aliases are matched; missing mappings are not invented.
    pub async fn groups_in_common(
        &self,
        jid: &str,
    ) -> Result<Vec<domain::SharedGroup>, domain::ServiceError> {
        crate::group_cache::groups_in_common(
            self.sqlite.shared(),
            self.device_id(),
            jid.to_string(),
        )
        .await
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
        self.hydrate_chat_avatars(&mut rows).await?;
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
        let mut rows =
            crate::chat_indexes::archived_page(self.shared_db(), self.device_id(), after, fetch)
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
        self.hydrate_chat_avatars(&mut rows).await?;
        let next_after =
            has_more.then(|| chat_summary_cursor(rows.last().expect("non-empty page")));
        Ok(domain::ChatPage { rows, next_after })
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
    /// and profile-photo identifiers remain `None` until an authoritative
    /// refresh writes them; the projection never invents values for them.
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
            is_blocked: None,
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

    pub async fn delete_local_contact(&self, jid: &str) -> Result<(), domain::ServiceError> {
        crate::contacts::delete_local(self.shared_db(), self.device_id(), jid.to_string()).await
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

    async fn hydrate_chat_avatars(
        &self,
        chats: &mut [domain::ChatSummary],
    ) -> Result<(), domain::ServiceError> {
        let mut contact_ids = Vec::new();
        let mut group_ids = Vec::new();
        for chat in chats.iter() {
            match chat.kind {
                domain::ChatKind::Direct => contact_ids.push(chat.id.as_str().to_string()),
                domain::ChatKind::Group => group_ids.push(chat.id.as_str().to_string()),
                domain::ChatKind::Newsletter | domain::ChatKind::System => {}
            }
        }
        let contacts =
            crate::contacts::load_avatar_refs(self.shared_db(), self.device_id(), contact_ids)
                .await?;
        let groups =
            crate::group_cache::load_avatar_refs(self.shared_db(), self.device_id(), group_ids)
                .await?;
        for chat in chats {
            let avatar = match chat.kind {
                domain::ChatKind::Direct => contacts.get(chat.id.as_str()),
                domain::ChatKind::Group => groups.get(chat.id.as_str()),
                domain::ChatKind::Newsletter | domain::ChatKind::System => None,
            };
            if let Some(avatar) = avatar {
                chat.avatar = Some(avatar.clone());
            }
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
        last_message_preview: chat_list_preview(
            e.last_message_kind.as_ref().map(|kind| kind.as_str()),
            e.last_message_preview,
        ),
        unread_count: e.unread_count as i64,
        pinned_at_ms: e.pinned_at.map(|t| t.timestamp_millis()),
        muted_until_ms: e.muted_until.map(|t| t.timestamp_millis()),
        archived: e.archived,
        favorite: false,
        draft_preview: None,
        draft: None,
        avatar: None,
    }
}

fn archived_chat_row_to_summary(row: crate::chat_indexes::ChatListRow) -> domain::ChatSummary {
    let kind = chat_kind(&row.jid);
    domain::ChatSummary {
        id: domain::ChatId::new(row.jid),
        kind,
        display_name: row.name,
        last_activity_ms: row.last_message_ts,
        last_message_preview: chat_list_preview(
            row.last_message_kind.as_deref(),
            row.last_message_preview,
        ),
        unread_count: row.unread_count as i64,
        pinned_at_ms: row.pinned_at,
        muted_until_ms: row.muted_until,
        archived: true,
        favorite: false,
        draft_preview: None,
        draft: None,
        avatar: None,
    }
}

fn chat_list_preview(kind: Option<&str>, stored: Option<String>) -> Option<String> {
    if stored
        .as_ref()
        .is_some_and(|preview| !preview.trim().is_empty())
    {
        return stored;
    }
    match kind {
        Some("location") => Some("Location".to_string()),
        Some("contact") => Some("Contact".to_string()),
        Some("poll") => Some("Poll".to_string()),
        _ => stored,
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

    use super::{
        aggregate_reactions, chat_kind, chat_list_preview, format_coord, map_kind_fields,
        map_quoted_message, notification_preview, quoted_preview,
    };
    use wasabi_domain::{ChatKind, MessageKind, UnavailableMessageReason};
    use whatsapp_rust::chrono::Utc;
    use whatsapp_rust::wacore::proto_helpers::{MessageBuilderExt, build_quote_context};
    use whatsapp_rust::waproto::buffa::MessageField;
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

    fn location_message(
        name: Option<&str>,
        address: Option<&str>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        live: bool,
    ) -> wa::Message {
        wa::Message {
            location_message: MessageField::some(wa::message::LocationMessage {
                degrees_latitude: latitude,
                degrees_longitude: longitude,
                name: name.map(str::to_string),
                address: address.map(str::to_string),
                is_live: live.then_some(true),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn live_location_message(
        caption: Option<&str>,
        latitude: Option<f64>,
        longitude: Option<f64>,
    ) -> wa::Message {
        wa::Message {
            live_location_message: MessageField::some(wa::message::LiveLocationMessage {
                degrees_latitude: latitude,
                degrees_longitude: longitude,
                caption: caption.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn contact_message(display_name: Option<&str>, vcard: Option<&str>) -> wa::Message {
        wa::Message {
            contact_message: MessageField::some(wa::message::ContactMessage {
                display_name: display_name.map(str::to_string),
                vcard: vcard.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn contacts_array_message(
        display_name: Option<&str>,
        contacts: Vec<(Option<&str>, Option<&str>)>,
    ) -> wa::Message {
        wa::Message {
            contacts_array_message: MessageField::some(wa::message::ContactsArrayMessage {
                display_name: display_name.map(str::to_string),
                contacts: contacts
                    .into_iter()
                    .map(|(name, vcard)| wa::message::ContactMessage {
                        display_name: name.map(str::to_string),
                        vcard: vcard.map(str::to_string),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    const PRIVATE_VCARD: &str =
        "BEGIN:VCARD\nVERSION:3.0\nFN:Secret Name\nTEL:+15555550100\nEND:VCARD";

    #[test]
    fn location_kind_projects_display_fields_without_raw_floats() {
        let message = location_message(
            Some("Harbor Park"),
            Some("12 Waterfront Way"),
            Some(37.808_000),
            Some(-122.409_500),
            false,
        );
        let kind = map_kind_fields("M", "location", None, Some(&message));
        assert_eq!(
            kind,
            MessageKind::Location {
                name: Some("Harbor Park".to_string()),
                address: Some("12 Waterfront Way".to_string()),
                latitude: Some("37.808".to_string()),
                longitude: Some("-122.4095".to_string()),
                live: false,
            }
        );
        assert_eq!(
            notification_preview(&kind),
            ("Location · Harbor Park".to_string(), true)
        );
        let debug = format!("{kind:?}");
        assert!(debug.contains("37.808"));
        assert!(!debug.contains("NaN"));
        assert_eq!(format_coord(Some(f64::NAN)), None);
        assert_eq!(format_coord(Some(f64::INFINITY)), None);
    }

    #[test]
    fn live_location_kind_is_labeled_without_claiming_a_moving_pin() {
        let message = live_location_message(Some("On the way"), Some(1.23), Some(4.56));
        let kind = map_kind_fields("M", "location", None, Some(&message));
        assert_eq!(
            kind,
            MessageKind::Location {
                name: None,
                address: Some("On the way".to_string()),
                latitude: Some("1.23".to_string()),
                longitude: Some("4.56".to_string()),
                live: true,
            }
        );
        assert_eq!(
            notification_preview(&kind),
            ("Live location".to_string(), true)
        );

        let flagged = location_message(None, None, Some(0.0), Some(0.0), true);
        let flagged_kind = map_kind_fields("M", "location", None, Some(&flagged));
        assert!(matches!(
            flagged_kind,
            MessageKind::Location { live: true, .. }
        ));
        assert_eq!(
            map_kind_fields("M", "location", None, None),
            MessageKind::Location {
                name: None,
                address: None,
                latitude: None,
                longitude: None,
                live: false,
            }
        );
    }

    #[test]
    fn contact_kind_uses_honest_fallbacks_and_strips_vcard() {
        let named = contact_message(Some("Jordan Blake"), Some(PRIVATE_VCARD));
        let kind = map_kind_fields("M", "contact", None, Some(&named));
        assert_eq!(
            kind,
            MessageKind::Contact {
                display_name: "Jordan Blake".to_string(),
                contacts: 1,
            }
        );
        let debug = format!("{kind:?}");
        assert!(!debug.contains("VCARD"));
        assert!(!debug.contains("15555550100"));
        assert!(!debug.contains("Secret Name"));
        assert_eq!(
            notification_preview(&kind),
            ("Jordan Blake".to_string(), true)
        );

        let unnamed = contact_message(Some("  "), Some(PRIVATE_VCARD));
        let fallback = map_kind_fields("M", "contact", None, Some(&unnamed));
        assert_eq!(
            fallback,
            MessageKind::Contact {
                display_name: "Contact".to_string(),
                contacts: 1,
            }
        );
        let fallback_debug = format!("{fallback:?}");
        assert!(!fallback_debug.contains("VCARD"));
        assert!(!fallback_debug.contains("15555550100"));
        assert_eq!(
            map_kind_fields("M", "contact", None, None),
            MessageKind::Contact {
                display_name: "Contact".to_string(),
                contacts: 1,
            }
        );
    }

    #[test]
    fn contacts_array_kind_counts_entries_and_keeps_vcard_off_the_boundary() {
        let message = contacts_array_message(
            Some("Weekend plans"),
            vec![
                (Some("Avery Chen"), Some(PRIVATE_VCARD)),
                (Some("Jordan Blake"), None),
            ],
        );
        let kind = map_kind_fields("M", "contact", None, Some(&message));
        assert_eq!(
            kind,
            MessageKind::Contact {
                display_name: "Weekend plans".to_string(),
                contacts: 2,
            }
        );
        assert!(!format!("{kind:?}").contains("VCARD"));
        assert!(!format!("{kind:?}").contains("15555550100"));

        let unnamed = contacts_array_message(None, vec![(None, Some(PRIVATE_VCARD))]);
        assert_eq!(
            map_kind_fields("M", "contact", None, Some(&unnamed)),
            MessageKind::Contact {
                display_name: "Contact".to_string(),
                contacts: 1,
            }
        );

        let empty_names = contacts_array_message(Some("   "), vec![(None, None), (None, None)]);
        assert_eq!(
            map_kind_fields("M", "contact", None, Some(&empty_names)),
            MessageKind::Contact {
                display_name: "Contacts".to_string(),
                contacts: 2,
            }
        );
    }

    #[test]
    fn quoted_preview_labels_location_and_contact_instead_of_generic_message() {
        let location = location_message(Some("Harbor Park"), None, Some(1.0), Some(2.0), false);
        assert_eq!(quoted_preview(&location), "Location · Harbor Park");
        assert_eq!(
            quoted_preview(&live_location_message(None, Some(1.0), Some(2.0))),
            "Live location"
        );
        assert_eq!(
            quoted_preview(&contact_message(Some("Jordan Blake"), Some(PRIVATE_VCARD))),
            "Jordan Blake"
        );
        assert!(
            !quoted_preview(&contact_message(Some("Jordan Blake"), Some(PRIVATE_VCARD)))
                .contains("VCARD")
        );
        assert_eq!(
            quoted_preview(&contacts_array_message(
                None,
                vec![(None, None), (None, None)]
            )),
            "Contacts"
        );
        assert_eq!(
            quoted_preview(&poll_message(
                Some("Weekend plans?"),
                &["Park", "Cinema"],
                Some(1),
                false,
                Some(vec![0xAA, 0xBB]),
                Some("Park"),
            )),
            "Weekend plans?"
        );
        assert_eq!(
            quoted_preview(&poll_message(None, &["A", "B"], Some(1), true, None, None)),
            "Quiz"
        );

        let reply = wa::Message::text_with_context(
            "on my way",
            build_quote_context(
                "LOC-ID",
                "15550000000@s.whatsapp.net",
                &location_message(Some("Harbor Park"), None, Some(1.0), Some(2.0), false),
            ),
        );
        let quoted = map_quoted_message(&reply).expect("quoted location");
        assert_eq!(quoted.id.as_str(), "LOC-ID");
        assert_eq!(quoted.preview, "Location · Harbor Park");
    }

    #[test]
    fn chat_list_preview_uses_kind_labels_when_store_text_is_empty() {
        assert_eq!(
            chat_list_preview(Some("location"), None).as_deref(),
            Some("Location")
        );
        assert_eq!(
            chat_list_preview(Some("contact"), Some(String::new())).as_deref(),
            Some("Contact")
        );
        assert_eq!(
            chat_list_preview(Some("location"), Some("Harbor Park".to_string())).as_deref(),
            Some("Harbor Park")
        );
        assert_eq!(
            chat_list_preview(Some("poll"), None).as_deref(),
            Some("Poll")
        );
        assert_eq!(
            chat_list_preview(Some("poll"), Some("Weekend plans?".to_string())).as_deref(),
            Some("Weekend plans?")
        );
        assert_eq!(chat_list_preview(Some("image"), None), None);
    }

    #[test]
    fn poll_kind_projects_names_without_keys_hashes_or_answers() {
        let message = poll_message(
            Some("Weekend plans?"),
            &["Park", "  ", "Cinema"],
            Some(2),
            false,
            Some(vec![0xDE, 0xAD]),
            Some("Park"),
        );
        let kind = map_kind_fields("M", "poll", None, Some(&message));
        assert_eq!(
            kind,
            MessageKind::Poll {
                name: "Weekend plans?".to_string(),
                options: vec!["Park".to_string(), "Cinema".to_string()],
                selectable_count: 2,
                quiz: false,
            }
        );
        assert_eq!(
            notification_preview(&kind),
            ("Weekend plans?".to_string(), true)
        );
        let debug = format!("{kind:?}");
        assert!(!debug.contains("DEAD"));
        assert!(!debug.contains("dead"));
        assert!(!debug.to_lowercase().contains("enc_key"));
        assert!(!debug.contains("option_hash"));

        let quiz = poll_message(None, &["Alpha", "Beta"], Some(0), true, None, Some("Alpha"));
        let quiz_kind = map_kind_fields("M", "poll", None, Some(&quiz));
        assert_eq!(
            quiz_kind,
            MessageKind::Poll {
                name: "Quiz".to_string(),
                options: vec!["Alpha".to_string(), "Beta".to_string()],
                selectable_count: 1,
                quiz: true,
            }
        );
        assert_eq!(notification_preview(&quiz_kind), ("Quiz".to_string(), true));

        assert_eq!(
            map_kind_fields("M", "poll", None, None),
            MessageKind::Poll {
                name: "Poll".to_string(),
                options: Vec::new(),
                selectable_count: 1,
                quiz: false,
            }
        );

        let v3 = wa::Message {
            poll_creation_message_v3: MessageField::some(wa::message::PollCreationMessage {
                name: Some("v3 question".to_string()),
                options: vec![
                    wa::message::poll_creation_message::Option {
                        option_name: Some("One".to_string()),
                        option_hash: Some("ffff".to_string()),
                    },
                    wa::message::poll_creation_message::Option {
                        option_name: Some("Two".to_string()),
                        option_hash: Some("eeee".to_string()),
                    },
                ],
                selectable_options_count: Some(1),
                ..Default::default()
            }),
            poll_creation_message: MessageField::some(wa::message::PollCreationMessage {
                name: Some("v1 should not win".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            map_kind_fields("M", "poll", None, Some(&v3)),
            MessageKind::Poll {
                name: "v3 question".to_string(),
                options: vec!["One".to_string(), "Two".to_string()],
                selectable_count: 1,
                quiz: false,
            }
        );
    }

    fn poll_message(
        name: Option<&str>,
        options: &[&str],
        selectable: Option<u32>,
        quiz: bool,
        enc_key: Option<Vec<u8>>,
        correct: Option<&str>,
    ) -> wa::Message {
        wa::Message {
            poll_creation_message: MessageField::some(wa::message::PollCreationMessage {
                name: name.map(str::to_string),
                options: options
                    .iter()
                    .map(|option| wa::message::poll_creation_message::Option {
                        option_name: Some((*option).to_string()),
                        option_hash: Some("deadbeef".to_string()),
                    })
                    .collect(),
                selectable_options_count: selectable,
                poll_type: quiz.then_some(wa::message::PollType::QUIZ),
                enc_key,
                correct_answer: match correct {
                    Some(answer) => {
                        MessageField::some(wa::message::poll_creation_message::Option {
                            option_name: Some(answer.to_string()),
                            option_hash: Some("cafebabe".to_string()),
                        })
                    }
                    None => MessageField::none(),
                },
                ..Default::default()
            }),
            ..Default::default()
        }
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
        .or_else(|| {
            base.location_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.live_location_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.contact_message
                .as_option()
                .and_then(|message| message.context_info.as_option())
        })
        .or_else(|| {
            base.contacts_array_message
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
    } else if let Some(preview) = quoted_location_preview(base) {
        preview
    } else if let Some(preview) = quoted_contact_preview(base) {
        preview
    } else if let Some(preview) = quoted_poll_preview(base) {
        preview
    } else {
        "Message".to_string()
    }
}

fn quoted_location_preview(base: &wa::Message) -> Option<String> {
    if let Some(live) = base.live_location_message.as_option() {
        return Some(location_preview_label(
            nonempty_text(live.caption.as_deref()).as_deref(),
            true,
        ));
    }
    let location = base.location_message.as_option()?;
    Some(location_preview_label(
        nonempty_text(location.name.as_deref()).as_deref(),
        location.is_live.unwrap_or(false),
    ))
}

fn quoted_contact_preview(base: &wa::Message) -> Option<String> {
    if let Some(array) = base.contacts_array_message.as_option() {
        return Some(contact_display_name(
            nonempty_text(array.display_name.as_deref()),
            array.contacts.len(),
        ));
    }
    let contact = base.contact_message.as_option()?;
    Some(contact_display_name(
        nonempty_text(contact.display_name.as_deref()),
        1,
    ))
}

fn location_preview_label(name: Option<&str>, live: bool) -> String {
    let kind = if live { "Live location" } else { "Location" };
    match name {
        Some(name) if name != kind => format!("{kind} · {name}"),
        _ => kind.to_string(),
    }
}

fn location_kind_preview(kind: &domain::MessageKind) -> String {
    match kind {
        domain::MessageKind::Location { name, live, .. } => {
            location_preview_label(name.as_deref(), *live)
        }
        _ => "Location".to_string(),
    }
}

fn contact_kind_preview(kind: &domain::MessageKind) -> String {
    match kind {
        domain::MessageKind::Contact { display_name, .. } => display_name.clone(),
        _ => "Contact".to_string(),
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
        domain::MessageKind::Location { .. } => (location_kind_preview(kind), true),
        domain::MessageKind::Contact { .. } => (contact_kind_preview(kind), true),
        domain::MessageKind::Poll { name, quiz, .. } => (poll_kind_preview(name, *quiz), true),
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
        "location" => map_location_kind(base),
        "contact" => map_contact_kind(base),
        "poll" => map_poll_kind(base),
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

fn map_location_kind(base: Option<&wa::Message>) -> domain::MessageKind {
    let live_wire = base.and_then(|message| message.live_location_message.as_option());
    let static_wire = base.and_then(|message| message.location_message.as_option());
    let live =
        live_wire.is_some() || static_wire.is_some_and(|location| location.is_live == Some(true));
    let (latitude, longitude, name, address) = if let Some(live) = live_wire {
        (
            live.degrees_latitude,
            live.degrees_longitude,
            None,
            nonempty_text(live.caption.as_deref()),
        )
    } else if let Some(location) = static_wire {
        (
            location.degrees_latitude,
            location.degrees_longitude,
            nonempty_text(location.name.as_deref()),
            nonempty_text(location.address.as_deref())
                .or_else(|| nonempty_text(location.comment.as_deref())),
        )
    } else {
        (None, None, None, None)
    };
    domain::MessageKind::Location {
        name,
        address,
        latitude: format_coord(latitude),
        longitude: format_coord(longitude),
        live,
    }
}

fn map_contact_kind(base: Option<&wa::Message>) -> domain::MessageKind {
    if let Some(array) = base.and_then(|message| message.contacts_array_message.as_option()) {
        let contacts = array.contacts.len().max(1);
        return domain::MessageKind::Contact {
            display_name: contact_display_name(
                nonempty_text(array.display_name.as_deref()).or_else(|| {
                    (array.contacts.len() == 1)
                        .then(|| array.contacts.first())
                        .flatten()
                        .and_then(|contact| nonempty_text(contact.display_name.as_deref()))
                }),
                array.contacts.len(),
            ),
            contacts,
        };
    }
    if let Some(contact) = base.and_then(|message| message.contact_message.as_option()) {
        return domain::MessageKind::Contact {
            display_name: contact_display_name(nonempty_text(contact.display_name.as_deref()), 1),
            contacts: 1,
        };
    }
    domain::MessageKind::Contact {
        display_name: "Contact".to_string(),
        contacts: 1,
    }
}

fn contact_display_name(name: Option<String>, contacts: usize) -> String {
    name.unwrap_or_else(|| {
        if contacts <= 1 {
            "Contact".to_string()
        } else {
            "Contacts".to_string()
        }
    })
}

fn map_poll_kind(base: Option<&wa::Message>) -> domain::MessageKind {
    let Some(poll) = first_poll_creation(base) else {
        return domain::MessageKind::Poll {
            name: poll_fallback_name(false),
            options: Vec::new(),
            selectable_count: 1,
            quiz: false,
        };
    };
    let quiz = poll.poll_type == Some(wa::message::PollType::QUIZ);
    let options = poll
        .options
        .iter()
        .filter_map(|option| nonempty_text(option.option_name.as_deref()))
        .collect();
    let selectable_count = poll
        .selectable_options_count
        .filter(|count| *count > 0)
        .unwrap_or(1);
    domain::MessageKind::Poll {
        name: nonempty_text(poll.name.as_deref()).unwrap_or_else(|| poll_fallback_name(quiz)),
        options,
        selectable_count,
        quiz,
    }
}

fn first_poll_creation(base: Option<&wa::Message>) -> Option<&wa::message::PollCreationMessage> {
    let base = base?;
    base.poll_creation_message_v3
        .as_option()
        .or_else(|| base.poll_creation_message_v2.as_option())
        .or_else(|| base.poll_creation_message.as_option())
}

fn quoted_poll_preview(base: &wa::Message) -> Option<String> {
    let poll = first_poll_creation(Some(base))?;
    let quiz = poll.poll_type == Some(wa::message::PollType::QUIZ);
    Some(nonempty_text(poll.name.as_deref()).unwrap_or_else(|| poll_fallback_name(quiz)))
}

fn poll_kind_preview(name: &str, quiz: bool) -> String {
    let name = name.trim();
    if name.is_empty() {
        poll_fallback_name(quiz)
    } else {
        name.to_string()
    }
}

fn poll_fallback_name(quiz: bool) -> String {
    if quiz {
        "Quiz".to_string()
    } else {
        "Poll".to_string()
    }
}

fn nonempty_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn format_coord(value: Option<f64>) -> Option<String> {
    let value = value?;
    if !value.is_finite() {
        return None;
    }
    let mut text = format!("{value:.6}");
    if let Some(dot) = text.find('.') {
        let keep = text[dot + 1..].trim_end_matches('0').len().max(1);
        text.truncate(dot + 1 + keep);
    }
    Some(text)
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

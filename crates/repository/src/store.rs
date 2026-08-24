//! Per-account storage: one shared SQLite database hosting both the protocol
//! store and the chat materialization store.
//!
//! This facade is the ONLY storage surface the rest of wasabi sees. ChatStore
//! types are mapped into domain projections at the edge; GPUI never touches
//! these structs directly.

use std::sync::Arc;

use tokio::sync::broadcast;

use wasabi_domain as domain;
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
        let next_after =
            has_more.then(|| chat_summary_cursor(rows.last().expect("non-empty page")));
        Ok(domain::ChatPage { rows, next_after })
    }

    /// The upstream store exposes active-only or active+archived scans. Walk
    /// that ordered keyset in bounded chunks and retain only archived rows so
    /// the product receives an honest archived-only destination without
    /// OFFSET pagination or a divergent SQL schema.
    async fn archived_chat_page(
        &self,
        after: Option<domain::ChatPageCursor>,
        limit: usize,
    ) -> Result<domain::ChatPage, domain::ServiceError> {
        const SCAN_CHUNK: usize = 128;

        let mut scan_after = after;
        let mut archived = Vec::with_capacity(limit.saturating_add(1));
        loop {
            let raw = self
                .chats
                .chats_page(
                    true,
                    scan_after.clone().map(domain_cursor_to_upstream),
                    SCAN_CHUNK as i64,
                )
                .await
                .map_err(database_error)?;
            let scanned = raw.len();
            if scanned == 0 {
                break;
            }
            for entry in raw {
                let summary = chat_entry_to_summary(entry);
                scan_after = Some(chat_summary_cursor(&summary));
                if summary.archived {
                    archived.push(summary);
                    if archived.len() > limit {
                        break;
                    }
                }
            }
            if archived.len() > limit || scanned < SCAN_CHUNK {
                break;
            }
        }

        let has_more = archived.len() > limit;
        archived.truncate(limit);
        let next_after =
            has_more.then(|| chat_summary_cursor(archived.last().expect("non-empty page")));
        Ok(domain::ChatPage {
            rows: archived,
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
        let out = rows
            .into_iter()
            .map(stored_to_row)
            .collect::<Result<Vec<_>, domain::ServiceError>>()?;
        Ok(domain::MessagePage {
            rows: out,
            next_before,
        })
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
        Ok(domain::DirectContactDetails {
            jid: jid.to_string(),
            display_name,
            phone_number,
            about: None,
            avatar: None,
        })
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
    use super::chat_kind;
    use wasabi_domain::ChatKind;

    #[test]
    fn chat_kind_is_derived_from_stable_jid_server() {
        assert_eq!(chat_kind("123@s.whatsapp.net"), ChatKind::Direct);
        assert_eq!(chat_kind("123@g.us"), ChatKind::Group);
        assert_eq!(chat_kind("123@newsletter"), ChatKind::Newsletter);
        assert_eq!(chat_kind("status@broadcast"), ChatKind::System);
    }
}

fn stored_to_row(
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
        status,
        edited_at_ms: m.edited_at.map(|t| t.timestamp_millis()),
        revoked: m.revoked,
        starred: m.starred,
    })
}

/// Map the stored kind + text into the UI-facing projection. Media payloads
/// stay behind handles added; nothing here carries bytes.
fn map_kind(m: &whatsapp_rust_chat_store::types::StoredMessage) -> domain::MessageKind {
    use whatsapp_rust_chat_store::types::MessageKind as K;
    match m.kind {
        K::Text => domain::MessageKind::Text {
            body: m.text.clone().unwrap_or_default(),
        },
        K::Image => domain::MessageKind::Image {
            caption: m.text.clone(),
            mime: None,
            media_key: None,
        },
        K::Video => domain::MessageKind::Video {
            caption: m.text.clone(),
            mime: None,
            media_key: None,
        },
        K::Audio => domain::MessageKind::Audio {
            mime: None,
            media_key: None,
        },
        K::Document => domain::MessageKind::Document {
            file_name: m.text.clone(),
            mime: None,
            media_key: None,
        },
        K::Sticker => domain::MessageKind::Sticker {
            mime: None,
            media_key: None,
        },
        K::Undecryptable | K::Unknown | K::Other(_) => domain::MessageKind::Unknown,
        _ => {
            // Reactions live in their own table (query via reactions()), and
            // the remaining kinds have no product surface yet.
            if m.text.is_some() {
                domain::MessageKind::System {
                    text: m.text.clone().unwrap_or_default(),
                }
            } else {
                domain::MessageKind::Unknown
            }
        }
    }
}

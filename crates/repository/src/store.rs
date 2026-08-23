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
    types::{ChatCursor, MessageCursor, StoreChange},
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

    pub fn device_id(&self) -> i32 {
        self.sqlite.device_id()
    }

    /// Subscribe to durable-change invalidations (bounded broadcast,
    /// capacity 256; lag ⇒ re-query —.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<StoreChange> {
        self.chats.subscribe()
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
        include_archived: bool,
        after: Option<domain::page::ChatPageCursor>,
        limit: usize,
    ) -> Result<Vec<domain::ChatSummary>, domain::ServiceError> {
        let after: Option<ChatCursor> = after.map(|c| ChatCursor {
            pinned_at_ms: c.pinned_at_ms,
            last_message_ts: c.last_activity_ms,
            jid: c.chat.as_str().to_string(),
        });
        let rows = self
            .chats
            .chats_page(include_archived, after, limit as i64)
            .await
            .map_err(|e| domain::ServiceError::new(domain::ErrorKind::Database, e.to_string()))?;
        Ok(rows.into_iter().map(chat_entry_to_summary).collect())
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
    domain::ChatSummary {
        id: domain::ChatId::new(e.jid.to_string()),
        display_name: e.name,
        last_activity_ms: e.last_message_at.map_or(0, |t| t.timestamp_millis()),
        last_message_preview: e.last_message_preview,
        unread_count: e.unread_count as i64,
        pinned_at_ms: e.pinned_at.map(|t| t.timestamp_millis()),
        muted_until_ms: e.muted_until.map(|t| t.timestamp_millis()),
        archived: e.archived,
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

//! History-sync materialization: turn one lazy `Event::HistorySync` payload
//! into committed chat-store rows, one account at a time.
//!
//! Chunking decision: the chat-store writer already streams every history
//! payload conversation-by-conversation inside its own transaction
//! (`apply_history_sync` walks `LazyHistorySync::stream()`), so this module
//! deliberately does NOT re-implement materialization or duplicate appliers:
//! it serializes behind a global gate, validates + measures the payload with
//! one read-only streaming pass, feeds the canonical event through the
//! store's own handler exactly like a live client delivery, and blocks on the
//! store's `flush` as the commit barrier.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use whatsapp_rust::types::events::{Event, LazyHistorySync};
use whatsapp_rust::wacore::history_sync::HistorySyncError;
use whatsapp_rust_chat_store::{ChatStore, ChatStoreError};

/// Refusal ceiling for a payload's DECLARED inflated size. Legitimate chunks
/// claim 5-20 MiB and wacore's unknown-size inflate ceiling is 64 MiB, so a
/// blob claiming half a GiB is corrupt or hostile metadata: reject it before
/// gating instead of trusting it enough to inflate toward it. The actual
/// inflate cap stays the exact declared size, enforced inside
/// `LazyHistorySync::stream()` — this constant only decides whether the
/// declaration is credible at all.
pub const MAX_DECOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HistoryError {
    /// This API never closes the gate semaphore, so this variant reports a
    /// bug elsewhere rather than hanging or panicking.
    #[error("history import gate closed")]
    GateClosed,

    #[error("history import cancelled")]
    Cancelled,

    #[error("payload declares {declared} decompressed bytes, over the {limit} ceiling")]
    Oversized { declared: u64, limit: u64 },

    /// The bounded writer queue refused the chunk, so nothing was
    /// materialized by this call; reporting success would be a lie.
    #[error("chat store writer queue refused the history chunk")]
    IngressFull,

    #[error("history sync payload unreadable: {0}")]
    Decode(#[from] HistorySyncError),

    #[error("chat store: {0}")]
    Store(#[from] ChatStoreError),
}

/// What one import did, safe to expose: counts and sizes only, never
/// conversation identities or message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportStats {
    /// Conversation entries the streaming pre-pass decoded.
    pub conversations: u64,
    /// Per-conversation wire message entries seen (including entries the
    /// store itself may skip as undecodable or placeholder rows).
    pub messages_seen: u64,
    /// Conversation entries skipped by the pre-pass because they failed to
    /// decode (mirrors the store's own leniency).
    pub skipped_conversations: u64,
    /// Compressed size of the largest batch handed to the writer — the bytes
    /// this module actually held in memory.
    pub peak_batch_bytes: u64,
    /// Wall time of the whole call, including gate wait.
    pub elapsed: Duration,
}

/// Global concurrency gate for history imports (one account at a time).
///
/// History chunks arrive as multi-chunk transfers whose server-side cursor
/// only advances cleanly when chunks apply in order; letting reconnects and
/// manual retries race each other interleaves two cursors over one store.
#[derive(Clone)]
pub struct HistoryGate {
    permit: Arc<Semaphore>,
}

impl HistoryGate {
    pub fn new() -> Self {
        Self {
            permit: Arc::new(Semaphore::new(1)),
        }
    }

    /// Run `f` while holding the single import slot. Cancellation while
    /// WAITING is immediate and side-effect free; cancellation DURING `f` is
    /// `f`'s own business (dropping it mid-await could strand enqueued-but-
    /// unflushed state), which is why `f` receives nothing and checks the
    /// token cooperatively.
    pub async fn run_import<F, T>(
        &self,
        token: CancellationToken,
        f: impl FnOnce() -> F,
    ) -> Result<T, HistoryError>
    where
        F: Future<Output = Result<T, HistoryError>>,
    {
        // biased: an already-cancelled token must win over a queued permit,
        // so a dead import never starts.
        let _permit = tokio::select! {
            biased;
            _ = token.cancelled() => return Err(HistoryError::Cancelled),
            acquired = self.permit.acquire() => acquired.map_err(|_| HistoryError::GateClosed)?,
        };
        // Released by dropping the permit when this future ends, success or not.
        f().await
    }
}

impl Default for HistoryGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Materialize one lazy history payload by feeding the store's OWN handler
/// path — bounded memory: never holds more than one conversation decoded at a
/// time; releases the payload's stream refs before enqueueing; refuses
/// declarations past [`MAX_DECOMPRESSED_BYTES`] and inflates toward the exact
/// declared size only.
///
/// Sequence: gate → read-only stats pre-pass (one bounded-window stream walk)
/// → `Event::HistorySync` into `chats.handler()` exactly as the client bus
/// would deliver it → `chats.flush()` commit barrier. Cancelling before the
/// enqueue leaves the store untouched (partial imports stay resumable
/// server-side); cancelling after it still waits for the flush, because the
/// bytes are already in the writer queue.
pub async fn import_lazy_history(
    chats: &Arc<ChatStore>,
    lazy: LazyHistorySync,
    gate: &HistoryGate,
    token: CancellationToken,
) -> Result<ImportStats, HistoryError> {
    if token.is_cancelled() {
        return Err(HistoryError::Cancelled);
    }
    let declared = lazy.decompressed_size() as u64;
    if declared > MAX_DECOMPRESSED_BYTES {
        return Err(HistoryError::Oversized {
            declared,
            limit: MAX_DECOMPRESSED_BYTES,
        });
    }
    let started = Instant::now();

    gate.run_import(token.clone(), move || async move {
        // Read-only observation pass over the compressed payload: the store
        // owns application, so this exists purely to fill ImportStats and to
        // fail fast on framing damage BEFORE handing anything to the writer.
        // Costs one extra inflate (tens of ms on multi-MB chunks) and buys
        // stats without reaching into store internals.
        let (conversations, messages_seen, skipped_conversations) = {
            // Inflate cap is the exact declared size (tighter than any global
            // ceiling), enforced by the stream itself.
            let mut stream = lazy.stream();
            let mut conversations = 0u64;
            let mut messages_seen = 0u64;
            loop {
                if token.is_cancelled() {
                    debug!("history: cancelled during pre-pass, nothing enqueued");
                    return Err(HistoryError::Cancelled);
                }
                match stream.next_conversation() {
                    Ok(Some(conv)) => {
                        conversations += 1;
                        messages_seen += conv.messages.len() as u64;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        return Err(HistoryError::Decode(e));
                    }
                }
            }
            let skipped_conversations = stream.skipped_conversations() as u64;
            match stream.remainder() {
                Ok(_) => {}
                // Optional-metadata tail (pushnames et al) failing to decode
                // must not void the conversations already validated; the
                // store applies this tail with the same leniency.
                Err(e) => warn!("history: payload tail unreadable, continuing: {e}"),
            }
            (conversations, messages_seen, skipped_conversations)
            // `stream` (and its inflate window) drops here, before the event
            // below takes the payload — peak memory stays one conversation.
        };

        let peak_batch_bytes = lazy.compressed_bytes().len() as u64;

        // Exactly the delivery shape of a live client: one canonical event,
        // refcount-cheap, applied by the store's writer inside its own
        // transactional batch. Content never touches this module's logs.
        let dropped_before = chats.ingress_dropped();
        chats
            .handler()
            .handle_event(Arc::new(Event::HistorySync(Box::new(lazy))));
        if chats.ingress_dropped() != dropped_before {
            return Err(HistoryError::IngressFull);
        }

        // Commit barrier: until this returns Ok the chunk may still roll back,
        // so success must not be reported before it lands. Awaited even when
        // the token fires mid-flush — the event is already enqueued.
        chats.flush().await?;

        info!(
            conversations,
            messages_seen, skipped_conversations, "history: chunk materialized"
        );
        Ok(ImportStats {
            conversations,
            messages_seen,
            skipped_conversations,
            peak_batch_bytes,
            elapsed: started.elapsed(),
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::id;
    use std::sync::atomic::{AtomicU64, Ordering};

    use whatsapp_rust::Jid;
    use whatsapp_rust::bytes::Bytes;
    use whatsapp_rust::waproto::buffa::{Message as _, MessageField};
    use whatsapp_rust::waproto::whatsapp as wa;
    use whatsapp_rust_sqlite_storage::SqliteStore;

    // Fictitious test JIDs, mirroring the chat-store fixtures.
    const PEER: &str = "559900000001@s.whatsapp.net";
    const OTHER: &str = "559900000002@s.whatsapp.net";

    async fn test_store() -> (SqliteStore, Arc<ChatStore>) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let db_name = format!(
            "file:memdb_history_{pid}_{n}?mode=memory&cache=shared",
            pid = id(),
        );
        let store = SqliteStore::new(&db_name).await.expect("create store");
        let chats = ChatStore::new(&store).await.expect("create chat store");
        (store, chats)
    }

    /// Spec-minimal zlib wrapper around stored DEFLATE blocks. flate2 is not
    /// a dependency of this crate and the fixture is tiny, so uncompressed
    /// deflate blocks exercise the identical inflate path dependency-free.
    fn zlib_stored(raw: &[u8]) -> Bytes {
        assert!(raw.len() <= u16::MAX as usize, "fixture must fit one block");
        let mut out = Vec::with_capacity(raw.len() + 16);
        // CMF=0x78 (deflate, 32K window), FLG=0x01: (0x78 << 8 | 1) % 31 == 0.
        out.extend_from_slice(&[0x78, 0x01]);
        // Single final stored block: BFINAL=1, BTYPE=00, LEN/NLEN LE.
        let len = raw.len() as u16;
        out.push(0x01);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(raw);
        // Adler-32 trailer, big-endian.
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in raw {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        Bytes::from(out)
    }

    /// One conversation carrying `msg_ids.len()` plain-text history messages.
    fn conversation(id: &str, msg_ids: &[&str]) -> wa::Conversation {
        wa::Conversation {
            id: id.to_string(),
            messages: msg_ids
                .iter()
                .enumerate()
                .map(|(i, mid)| wa::HistorySyncMsg {
                    message: MessageField::some(wa::WebMessageInfo {
                        key: MessageField::some(wa::MessageKey {
                            id: Some((*mid).to_string()),
                            from_me: Some(false),
                            ..Default::default()
                        }),
                        message_timestamp: Some(1_700_000_000 + i as u64),
                        message: MessageField::some(wa::Message {
                            conversation: Some("historic hello".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn lazy_fixture(conversations: Vec<wa::Conversation>) -> LazyHistorySync {
        let hs = wa::HistorySync {
            sync_type: wa::history_sync::HistorySyncType::InitialBootstrap,
            conversations,
            ..Default::default()
        };
        let raw = hs.encode_to_vec();
        let len = raw.len();
        LazyHistorySync::new(
            zlib_stored(&raw),
            len,
            wa::history_sync::HistorySyncType::InitialBootstrap as i32,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn import_materializes_rows_and_reports_stats() {
        let (_store, chats) = test_store().await;
        let gate = HistoryGate::new();

        let stats = import_lazy_history(
            &chats,
            lazy_fixture(vec![
                conversation(PEER, &["HIST-1", "HIST-2"]),
                conversation(OTHER, &["HIST-3"]),
            ]),
            &gate,
            CancellationToken::new(),
        )
        .await
        .expect("import succeeds");

        assert_eq!(stats.conversations, 2);
        assert_eq!(stats.messages_seen, 3);
        assert_eq!(stats.skipped_conversations, 0);
        assert!(stats.peak_batch_bytes > 0);

        let peer: Jid = PEER.parse().expect("valid test JID");
        let page = chats.messages(&peer, None, 10).await.expect("messages");
        assert_eq!(page.len(), 2);
        let mut ids: Vec<&str> = page.iter().map(|m| m.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["HIST-1", "HIST-2"]);
        assert!(
            page.iter()
                .all(|m| m.text.as_deref() == Some("historic hello"))
        );

        // The second conversation routed to its own thread.
        let other_page = chats
            .messages(&OTHER.parse().expect("valid test JID"), None, 10)
            .await
            .expect("messages");
        assert_eq!(other_page.len(), 1);
        assert_eq!(other_page[0].id, "HIST-3");
    }

    #[tokio::test]
    async fn cancelled_import_enqueues_nothing() {
        let (_store, chats) = test_store().await;
        let gate = HistoryGate::new();
        let token = CancellationToken::new();
        token.cancel();

        let result = import_lazy_history(
            &chats,
            lazy_fixture(vec![conversation(PEER, &["HIST-CANCEL"])]),
            &gate,
            token,
        )
        .await;

        assert!(
            matches!(result, Err(HistoryError::Cancelled)),
            "expected cancellation"
        );
        let peer: Jid = PEER.parse().expect("valid test JID");
        let page = chats.messages(&peer, None, 10).await.expect("messages");
        assert!(
            page.is_empty(),
            "cancelled import must not materialize rows"
        );
    }
}

//! Durable outgoing messages: record-before-send with a commit barrier.
//!
//! The ordering is the correctness contract. The id is minted first, then the
//! message is durably enqueued into the chat store and the store's commit
//! barrier (`flush`) is awaited BEFORE anything goes on the wire. A crash
//! between barrier and publish leaves a committed `Pending` row, which the
//! startup sweep ([`reconcile_stale_pending`]) resends under the SAME id —
//! the server dedupes by id, so the resend is idempotent. The reverse order
//! would be unfixable: a published message with no durable row can never be
//! reconciled, because its content is gone.

use std::sync::Arc;

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use whatsapp_rust::chrono::{DateTime, Duration, Utc};
use whatsapp_rust::client::Client;
use whatsapp_rust::types::events::{Event, ServerAck};
use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{Jid, SendError, SendOptions};
use whatsapp_rust_chat_store::{
    ChatStore, ChatStoreError, MessageCursor, MessageStatus, types::StoredMessage,
};

/// Confirmation that a message passed the full pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentReceipt {
    /// The id the message was recorded AND published under — the key every
    /// later ack/receipt/status update correlates on.
    pub message_id: String,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OutboxError {
    /// No live transport when the pipeline was entered. Nothing was recorded:
    /// minting ids and committing rows for an obviously-offline client would
    /// just queue reconciliation work for a message the caller still holds.
    #[error("not connected")]
    NotConnected,

    #[error("chat store: {0}")]
    Store(#[from] ChatStoreError),

    /// The message IS durably recorded (the commit barrier passed); only the
    /// network publication failed. The row was marked locally failed, and the
    /// id lets the caller offer a manual retry against the stored content.
    #[error("send failed for {message_id}")]
    Send {
        message_id: String,
        #[source]
        source: SendError,
    },

    /// The request was rejected before anything durable happened.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

/// What a send failure means for retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Transient transport trouble; an automatic resend of the same id is
    /// safe and likely to succeed.
    SafeAutoRetry,
    /// Retry is worthwhile only after some state settles (login, device-list
    /// refresh, reconnect) — the reconcile sweep or a reconnected client
    /// should drive it, not a tight loop.
    NeedsReconciliation,
    /// Unknown internal cause; do not retry automatically. Surface to the
    /// user/log and let a human decide.
    ManualRetry,
    /// Deterministic rejection; the identical request fails identically.
    Permanent,
}

/// Classify a library send failure by its kind. `SendError` and the inner
/// `ClientError` are `#[non_exhaustive]`, so unknown future variants land in
/// [`FailureClass::ManualRetry`] — the conservative default for automation.
pub fn classify_send_failure(e: &SendError) -> FailureClass {
    use whatsapp_rust::client::ClientError;

    match e {
        // Transport-level failures: nothing (or an idempotent prefix) reached
        // the server, so resending the same id converges.
        SendError::Client(err) => match err {
            ClientError::NotConnected | ClientError::Socket(_) | ClientError::EncryptSend(_) => {
                FailureClass::SafeAutoRetry
            }
            // Identity or a prerequisite IQ not settled yet; a retry makes
            // sense once the session re-establishes itself.
            ClientError::NotLoggedIn | ClientError::Iq(_) => FailureClass::NeedsReconciliation,
            _ => FailureClass::ManualRetry,
        },
        SendError::NotLoggedIn => FailureClass::NeedsReconciliation,
        // A group-metadata-style IQ failed before publication; the send
        // itself never started, so a plain retry is safe.
        SendError::Iq(_) => FailureClass::SafeAutoRetry,
        // Recipient device list went stale mid-handshake: refresh devices,
        // then a resend resolves it.
        SendError::NoRecipientDevice(_) => FailureClass::NeedsReconciliation,
        SendError::Internal(_) => FailureClass::ManualRetry,
        SendError::InvalidRequest(_) => FailureClass::Permanent,
        _ => FailureClass::ManualRetry,
    }
}

/// The sending side of one account: every outgoing message goes through
/// here so "durable first" cannot be bypassed by accident.
#[derive(Clone)]
pub struct Outbox {
    chats: Arc<ChatStore>,
}

impl Outbox {
    pub fn new(chats: Arc<ChatStore>) -> Self {
        Self { chats }
    }

    /// Plain text over [`Self::send_message`].
    pub async fn send_text(
        &self,
        client: &Arc<Client>,
        to: Jid,
        text: String,
    ) -> Result<SentReceipt, OutboxError> {
        // Validate before anything durable: an empty body is a caller bug,
        // and a committed Pending row for it would fail forever.
        if text.trim().is_empty() {
            return Err(OutboxError::InvalidRequest("text is empty".into()));
        }
        self.send_inner(client, to, wa::Message::text(text)).await
    }

    /// Full pipeline. The order keeps durable writes ahead of transport work.
    pub async fn send_message(
        &self,
        client: &Arc<Client>,
        to: Jid,
        message: wa::Message,
    ) -> Result<SentReceipt, OutboxError> {
        self.send_inner(client, to, message).await
    }

    /// Republish one durable failed outgoing row under its original message
    /// id. Reusing the id is essential: the server can deduplicate an
    /// ambiguous earlier attempt, while minting a new id could duplicate the
    /// user's message.
    pub async fn retry_failed(
        &self,
        client: &Arc<Client>,
        chat: Jid,
        message_id: &str,
    ) -> Result<SentReceipt, OutboxError> {
        if !client.is_connected() {
            return Err(OutboxError::NotConnected);
        }
        let stored = self
            .chats
            .message(&chat, message_id)
            .await?
            .ok_or_else(|| OutboxError::InvalidRequest("message no longer exists".into()))?;
        validate_retry_candidate(&stored)?;
        let message = stored
            .message
            .as_deref()
            .ok_or_else(|| OutboxError::InvalidRequest("message content is unavailable".into()))?;
        let options = SendOptions::default().with_message_id(stored.id.clone());
        match client
            .send_message_with_options(chat.clone(), message.clone(), options)
            .await
        {
            Ok(_) => Ok(SentReceipt {
                message_id: stored.id,
            }),
            Err(source) => {
                warn!(
                    id = %stored.id,
                    class = ?classify_send_failure(&source),
                    error = %source,
                    "outbox: manual retry failed"
                );
                Err(OutboxError::Send {
                    message_id: stored.id,
                    source,
                })
            }
        }
    }

    async fn send_inner(
        &self,
        client: &Arc<Client>,
        to: Jid,
        message: wa::Message,
    ) -> Result<SentReceipt, OutboxError> {
        if !client.is_connected() {
            return Err(OutboxError::NotConnected);
        }

        // Minted BEFORE the record so the durable row and the wire stanza
        // share one identity from the first byte.
        let id = client.generate_message_id();

        // Backpressuring enqueue: under a stalled writer this awaits instead
        // of refusing, because losing the enqueue loses the message.
        self.chats.record_outgoing(&to, &id, &message, Utc::now())?;

        // COMMIT BARRIER. Until this returns Ok the batch may still roll
        // back, so publishing first could put a message on the wire with no
        // durable trace; after it returns Ok every failure below is
        // recoverable via the reconcile sweep or a manual retry.
        if let Err(e) = self.chats.flush().await {
            return Err(OutboxError::Store(e));
        }

        // Struct literals are barred by #[non_exhaustive]; builder-style it is.
        let options = SendOptions::default().with_message_id(id.clone());
        match client
            .send_message_with_options(to.clone(), message, options)
            .await
        {
            Ok(result) => {
                if result.message_id != id {
                    debug!(
                        id = %id,
                        reported = %result.message_id,
                        "outbox: server-reported id differs from pinned id"
                    );
                }
                Ok(SentReceipt { message_id: id })
            }
            Err(e) => {
                warn!(
                    id = %id,
                    chat = %to.to_string(),
                    class = ?classify_send_failure(&e),
                    error = %e,
                    "outbox: publish failed after durable commit"
                );
                // Only lifts a still-Pending row to ERROR; a late ack racing
                // this call wins, which is the correct outcome.
                if let Err(mark_err) = record_send_failure(&self.chats, &to, &id).await {
                    warn!(id = %id, error = %mark_err, "outbox: marking send-failed failed");
                }
                Err(OutboxError::Send {
                    message_id: id,
                    source: e,
                })
            }
        }
    }
}

fn validate_retry_candidate(message: &StoredMessage) -> Result<(), OutboxError> {
    if !message.from_me {
        return Err(OutboxError::InvalidRequest(
            "incoming messages cannot be retried".into(),
        ));
    }
    if message.status != MessageStatus::Error {
        return Err(OutboxError::InvalidRequest(
            "only failed messages can be retried".into(),
        ));
    }
    if message.message.is_none() {
        return Err(OutboxError::InvalidRequest(
            "message content is unavailable".into(),
        ));
    }
    Ok(())
}

async fn record_send_failure(
    chats: &ChatStore,
    chat: &Jid,
    message_id: &str,
) -> Result<(), ChatStoreError> {
    let event = Event::ServerAck(
        ServerAck::builder()
            .id(message_id.to_owned())
            .class("message".to_owned())
            .from(chat.clone())
            .error("local send failure".to_owned())
            .build(),
    );
    chats.handler().handle_event(Arc::new(event));
    chats.flush().await
}

/// Backward-scan bounds. The scan cap keeps pathological histories cheap;
/// the sent-run break exploits locality — a run of recent sends that all
/// completed means the unsent tail (if any) is close behind.
const RECONCILE_SCAN_CAP: usize = 500;
const RECONCILE_PAGE_SIZE: i64 = 100;
/// Consecutive from-me rows already past `Pending` that end one chat's walk.
const SENT_RUN_BREAK: usize = 5;
/// Upper bound on chats inspected per pass; larger accounts simply cover
/// their remainder on the next scheduled pass.
const RECONCILE_CHAT_CAP: i64 = 500;

/// Startup sweep: publish this account's committed-but-unpublished messages.
///
/// Collects `from_me` rows still in [`MessageStatus::Pending`] older than
/// `stale_after` by paging each chat backwards, then resends each ONE attempt
/// under its ORIGINAL id — never a new id, because the durable row and any
/// server-side dedupe state are keyed by it. A row whose stored proto is
/// missing cannot be re-materialized and is marked failed rather than
/// silently dropped. Every resend failure ends in a durable error marker: the
/// sweep never loops on a sick recipient, and the user retries manually.
///
/// Single concurrent instance is the CALLER's obligation (one spawned task
/// per session). Cancelling mid-send drops that send future: the stanza may
/// or may not have reached the server, which is exactly the ambiguous state
/// the next sweep resolves by resending the same id.
pub async fn reconcile_stale_pending(
    chats: Arc<ChatStore>,
    client: Arc<Client>,
    stale_after: Duration,
    token: CancellationToken,
) {
    if token.is_cancelled() {
        return;
    }
    // Running while disconnected would mark every pending row failed over a
    // mere transport hiccup; bail so the caller can reschedule once live.
    if !client.is_connected() {
        info!("outbox: reconcile skipped, client not connected");
        return;
    }

    // One cutoff instant for the whole pass: rows are compared against when
    // the sweep started, not against a clock drifting under its feet.
    let cutoff: DateTime<Utc> = Utc::now() - stale_after;

    let chat_list = match chats.chats(true, RECONCILE_CHAT_CAP).await {
        Ok(list) => list,
        Err(e) => {
            warn!(error = %e, "outbox: reconcile could not list chats");
            return;
        }
    };

    let mut stale: Vec<StoredMessage> = Vec::new();
    'chats: for entry in chat_list {
        if token.is_cancelled() {
            return;
        }
        let mut before: Option<MessageCursor> = None;
        let mut scanned = 0usize;
        let mut sent_run = 0usize;
        loop {
            if token.is_cancelled() {
                return;
            }
            let page = match chats
                .messages(&entry.jid, before.clone(), RECONCILE_PAGE_SIZE)
                .await
            {
                Ok(page) => page,
                Err(e) => {
                    warn!(chat = %entry.jid.to_string(), error = %e, "outbox: reconcile page read failed");
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            for m in &page {
                scanned += 1;
                if !m.from_me {
                    continue;
                }
                if m.status == MessageStatus::Pending {
                    sent_run = 0;
                    if m.timestamp <= cutoff {
                        stale.push(m.clone());
                    }
                } else {
                    sent_run += 1;
                    if sent_run >= SENT_RUN_BREAK {
                        // Everything newer than here was published; this
                        // chat has no unsent tail worth paging further for.
                        continue 'chats;
                    }
                }
                if scanned >= RECONCILE_SCAN_CAP {
                    continue 'chats;
                }
            }
            before = page.last().map(MessageCursor::from);
            if page.len() < RECONCILE_PAGE_SIZE as usize || scanned >= RECONCILE_SCAN_CAP {
                break;
            }
        }
    }

    if stale.is_empty() {
        debug!("outbox: reconcile found nothing pending");
        return;
    }
    // Oldest first so conversation order is restored in sequence, not
    // scrambled by whatever order the per-chat scans happened to meet.
    stale.sort_by_key(|m| (m.timestamp, m.seq));
    info!(
        count = stale.len(),
        "outbox: resending stale pending messages"
    );

    for m in stale {
        if token.is_cancelled() {
            info!("outbox: reconcile cancelled with messages left pending");
            return;
        }
        let id = m.id.clone();
        let Some(body) = m.message.as_deref() else {
            // No stored proto means no content to re-materialize; fail the
            // row honestly instead of leaving a zombie Pending forever.
            warn!(id = %id, chat = %m.chat_jid.to_string(), "outbox: stale message has no stored proto");
            if let Err(e) = record_send_failure(&chats, &m.chat_jid, &id).await {
                warn!(id = %id, error = %e, "outbox: marking send-failed failed");
            }
            continue;
        };
        // Struct literals are barred by #[non_exhaustive]; builder-style it is.
        let options = SendOptions::default().with_message_id(id.clone());
        let send = client.send_message_with_options(m.chat_jid.clone(), body.clone(), options);
        tokio::select! {
            _ = token.cancelled() => {
                info!(id = %id, "outbox: reconcile cancelled mid-send");
                return;
            }
            result = send => match result {
                Ok(_) => info!(id = %id, chat = %m.chat_jid.to_string(), "outbox: resent stale pending message"),
                Err(e) => {
                    warn!(
                        id = %id,
                        chat = %m.chat_jid.to_string(),
                        class = ?classify_send_failure(&e),
                        error = %e,
                        "outbox: stale resend failed"
                    );
                    if let Err(mark_err) = record_send_failure(&chats, &m.chat_jid, &id).await {
                        warn!(id = %id, error = %mark_err, "outbox: marking send-failed failed");
                    }
                }
            }
        }
    }

    // Best-effort durability of the failure markers produced above; the
    // writer commits them soon regardless, but a clean exit should not
    // depend on that grace window.
    if let Err(e) = chats.flush().await {
        warn!(error = %e, "outbox: reconcile final flush failed");
    }
}

#[cfg(test)]
mod tests {
    use super::{OutboxError, validate_retry_candidate};
    use whatsapp_rust::chrono::Utc;
    use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
    use whatsapp_rust::waproto::whatsapp as wa;
    use whatsapp_rust::Jid;
    use whatsapp_rust_chat_store::{MessageKind, MessageStatus, types::StoredMessage};

    fn retry_candidate(from_me: bool, status: MessageStatus, with_content: bool) -> StoredMessage {
        let chat: Jid = "15550000000@s.whatsapp.net".parse().unwrap();
        StoredMessage {
            chat_jid: chat.clone(),
            id: "FAILED-1".to_string(),
            sender_jid: chat,
            from_me,
            timestamp: Utc::now(),
            kind: MessageKind::Text,
            text: Some("retry me".to_string()),
            message: with_content.then(|| Box::new(wa::Message::text("retry me"))),
            status,
            starred: false,
            edited_at: None,
            revoked: false,
            seq: 1,
        }
    }

    #[test]
    fn manual_retry_requires_a_failed_outgoing_row_with_durable_content() {
        assert!(
            validate_retry_candidate(&retry_candidate(true, MessageStatus::Error, true)).is_ok()
        );
        for candidate in [
            retry_candidate(false, MessageStatus::Error, true),
            retry_candidate(true, MessageStatus::Pending, true),
            retry_candidate(true, MessageStatus::Error, false),
        ] {
            assert!(matches!(
                validate_retry_candidate(&candidate),
                Err(OutboxError::InvalidRequest(_))
            ));
        }
    }
}

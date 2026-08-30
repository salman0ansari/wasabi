//! One place mapping protocol events onto wasabi's domain surfaces: coarse
//! [`Invalidation`]s for UI projections and [`SessionState`] transitions for
//! the session machine.
//!
//! Pure functions — no I/O, no channels: callers own delivery. Protocol types
//! stay inside this crate, so GPUI never sees a `waproto` type.
//!
//! `State` signals are *proposals*, not commits. This module cannot know the
//! session's current state, so it returns target states only; the caller runs
//! each through [`SessionState::transition`] against the live state and drops
//! rejected ones (`lifecycle::transition_to`). Multi-hop arrivals (e.g.
//! relink landing on `Connected` while the machine sits in `Pairing`) are the
//! caller's concern: walk the table, inserting intermediate hops as needed.
//!
//! The QR payload itself never crosses this layer — only the fact that a
//! code arrived and how long it lives. Debug output redacts JIDs, mirroring
//! the upstream event bus's variant-name-only `Debug` discipline.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use wasabi_core::events::Invalidation;
use wasabi_core::state::{SessionState, failure_reason};
use whatsapp_rust::Jid;
use whatsapp_rust::types::events::{ConnectFailureReason, Event, LoggedOut, TemporaryBan};

/// What the UI should do about one protocol event.
#[derive(Clone, PartialEq, Eq)]
pub enum UiSignal {
    /// A durable domain changed; re-query the projection.
    Invalidated(Invalidation),
    /// Proposed session state; callers apply it through the session reducer.
    State(SessionState),
    /// A pairing code arrived; `expires_in` is the library-quoted validity
    /// window. The code itself is fetched by the caller from its own channel.
    QrArrived { expires_in: Duration },
    /// Whatever code was on screen is now dead (rotated, consumed, failed).
    QrCleared,
    /// QR rotation gave up; pairing needs a fresh connection attempt.
    PairingExhausted,
}

impl fmt::Debug for UiSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Manual impl so JIDs never reach logs through a stray `{:?}`.
        // `SessionState` reasons are static labels or transport-level text,
        // and the QR code physically is not carried here.
        match self {
            Self::Invalidated(Invalidation::Messages { chat }) => f
                .debug_struct("Invalidated")
                .field("Messages.chat", &redact_jid(chat))
                .finish(),
            Self::Invalidated(inv @ (Invalidation::Chats | Invalidation::Contacts)) => {
                f.debug_tuple("Invalidated").field(inv).finish()
            }
            Self::State(state) => f.debug_tuple("State").field(state).finish(),
            Self::QrArrived { expires_in } => f
                .debug_struct("QrArrived")
                .field("expires_in", expires_in)
                .finish(),
            Self::QrCleared => f.write_str("QrCleared"),
            Self::PairingExhausted => f.write_str("PairingExhausted"),
        }
    }
}

/// Stable [`SessionState::Failed`] label for a server-driven logout.
/// `is_logged_out()` is a normal unlink; anything else is a forced removal.
#[must_use]
pub(crate) fn logged_out_reason(logout: &LoggedOut) -> &'static str {
    if logout.reason.is_logged_out() {
        failure_reason::LOGGED_OUT
    } else {
        failure_reason::FORCED_LOGOUT
    }
}

#[must_use]
pub(crate) fn logged_out_state(logout: &LoggedOut) -> SessionState {
    SessionState::Failed {
        reason: logged_out_reason(logout).to_string(),
    }
}

#[must_use]
pub(crate) fn client_outdated_state() -> SessionState {
    SessionState::Failed {
        reason: failure_reason::CLIENT_OUTDATED.to_string(),
    }
}

#[must_use]
pub(crate) fn stream_replaced_state() -> SessionState {
    SessionState::Failed {
        reason: failure_reason::STREAM_REPLACED.to_string(),
    }
}

/// Map a connect-failure reason onto a session state without `Debug`.
///
/// Reconnectable codes stay with the library retry loop. A 429 (or
/// `Unknown(429)`) is a stable `"rate limited"` label. A temp-ban without an
/// expiry arrives here instead of [`Event::TemporaryBan`] — do not invent a
/// wait window.
#[must_use]
pub(crate) fn connect_failure_state(reason: ConnectFailureReason) -> SessionState {
    if reason.should_reconnect() {
        SessionState::Reconnecting
    } else if is_rate_limited_connect(reason) {
        SessionState::Failed {
            reason: failure_reason::RATE_LIMITED.to_string(),
        }
    } else if matches!(reason, ConnectFailureReason::TempBanned) {
        SessionState::Failed {
            reason: failure_reason::TEMPORARILY_BANNED.to_string(),
        }
    } else if matches!(reason, ConnectFailureReason::ClientOutdated) {
        client_outdated_state()
    } else if reason.is_logged_out() {
        SessionState::Failed {
            reason: failure_reason::LOGGED_OUT.to_string(),
        }
    } else {
        SessionState::Failed {
            reason: failure_reason::CONNECT_FAILURE.to_string(),
        }
    }
}

fn is_rate_limited_connect(reason: ConnectFailureReason) -> bool {
    reason.code() == 429
}

/// Encode a temporary ban as a stable label plus the server's wait, in
/// seconds. A missing expiry never reaches this event (the library emits
/// [`Event::ConnectFailure`] instead).
#[must_use]
pub(crate) fn temporary_ban_state(ban: &TemporaryBan) -> SessionState {
    SessionState::Failed {
        reason: failure_reason::temporarily_banned(ban.expire.num_seconds()),
    }
}

/// Classify one protocol event into UI signals. Exhaustive over `Event`;
/// the catch-all keeps unknown future variants a silent no-op rather than a
/// compile break in callers.
#[must_use]
pub fn classify(event: &Event) -> Vec<UiSignal> {
    match event {
        // --- Connection lifecycle: state proposals ----------------------
        Event::Connected(_) => vec![UiSignal::State(SessionState::Connected)],
        Event::Disconnected(d) => {
            // A clean server recycle hands straight back to the reconnect
            // loop; anything louder deserves its own visible label.
            if d.reason.is_clean_shutdown() {
                vec![UiSignal::State(SessionState::Reconnecting)]
            } else {
                vec![UiSignal::State(SessionState::Disconnected {
                    reason: Some(d.reason.to_string()),
                })]
            }
        }
        Event::StreamError(_) => vec![UiSignal::State(SessionState::Reconnecting)],
        Event::StreamReplaced(_) => vec![UiSignal::State(stream_replaced_state())],
        Event::LoggedOut(logout) => vec![UiSignal::State(logged_out_state(logout))],
        Event::ConnectFailure(failure) => {
            vec![UiSignal::State(connect_failure_state(failure.reason))]
        }
        Event::TemporaryBan(ban) => vec![UiSignal::State(temporary_ban_state(ban))],
        Event::ClientOutdated(_) => vec![UiSignal::State(client_outdated_state())],
        Event::PairSuccess(_) => {
            // Pairing succeeded and the stack goes straight at its first
            // connect; the QR screen has nothing left to show.
            vec![
                UiSignal::State(SessionState::Connecting),
                UiSignal::QrCleared,
            ]
        }

        // --- Pairing surfaces -------------------------------------------
        Event::PairingQrCode(qr) => {
            vec![UiSignal::QrArrived {
                expires_in: qr.timeout,
            }]
        }
        Event::PairingCode(code) => {
            vec![UiSignal::QrArrived {
                expires_in: code.timeout,
            }]
        }
        Event::PairingCodeRefresh(_) => {
            // The outstanding code was invalidated before a replacement.
            vec![UiSignal::QrCleared]
        }
        Event::PairingCodeError(_) => {
            // The phone-number code is finished; whatever was displayed dies.
            // Retry policy (backoff/rejection detail) travels with the
            // caller's own request result, not through UI signals.
            vec![UiSignal::QrCleared]
        }
        Event::QrScannedWithoutMultidevice(_) => {
            // Scanned but refused: the rendered code is spent regardless.
            vec![UiSignal::QrCleared]
        }
        Event::PairingQrCodesExhausted(_) => vec![UiSignal::PairingExhausted],

        // --- Messages: per-chat invalidations ---------------------------
        Event::Messages(batch) => {
            // Distinct chats only: a drain batch spanning one conversation N
            // times must not fire N re-queries. hook_committed batches still
            // invalidate — committed rows are visible rows.
            let mut chats: Vec<String> = Vec::new();
            for msg in batch.iter() {
                if let Some(chat) = chat_of(&msg.info.source.chat)
                    && !chats.contains(&chat)
                {
                    chats.push(chat);
                }
            }
            match (batch.is_empty(), chats.is_empty()) {
                (true, _) => vec![],
                // Content arrived but no chat survived parsing; fall back to
                // the coarse sweep rather than dropping the signal entirely.
                (false, true) => vec![UiSignal::Invalidated(Invalidation::Chats)],
                (false, false) => chats
                    .into_iter()
                    .map(|chat| UiSignal::Invalidated(Invalidation::Messages { chat }))
                    .collect(),
            }
        }
        Event::UndecryptableMessage(msg) => {
            // A placeholder row lands in the chat's message set.
            vec![UiSignal::Invalidated(scoped(&msg.info.source.chat))]
        }
        Event::Receipt(receipt) => {
            // Every receipt flavor mutates message-row state (delivery/read/
            // played ticks), so one scoped invalidation covers it. The chat
            // rides the full `MessageSource`; an unparsed one falls back.
            vec![UiSignal::Invalidated(scoped(&receipt.source.chat))]
        }
        Event::ServerAck(ack) => {
            // Acks ride every outgoing stanza class (message, receipt,
            // notification, call, …); only message acks mean a chat's rows
            // moved. `from` echoes the chat when the server included it.
            if ack.class.as_deref() != Some("message") {
                vec![]
            } else {
                vec![UiSignal::Invalidated(match ack.from.as_ref().and_then(chat_of) {
                    Some(chat) => Invalidation::Messages { chat },
                    None => Invalidation::Chats,
                })]
            }
        }

        // --- Contact/profile naming -------------------------------------
        Event::PictureUpdate(_)
        | Event::UserAboutUpdate(_)
        | Event::BusinessStatusUpdate(_)
        | Event::SelfPushNameUpdated(_) => vec![contacts()],
        Event::ContactUpdated(_)
        | Event::ContactNumberChanged(_)
        | Event::ContactSyncRequested(_)
        | Event::ContactRemoved(_)
        | Event::ContactUpdate(_) => vec![contacts()],

        // --- Chat metadata: row-level churn on the chat list -------------
        Event::PinUpdate(upd) => chat_row_signals(&upd.jid),
        Event::MuteUpdate(upd) => chat_row_signals(&upd.jid),
        Event::ArchiveUpdate(upd) => chat_row_signals(&upd.jid),
        Event::MarkChatAsReadUpdate(upd) => chat_row_signals(&upd.jid),
        Event::DeleteChatUpdate(upd) => chat_row_signals(&upd.jid),
        Event::ClearChatUpdate(upd) => chat_row_signals(&upd.jid),
        Event::UserStatusMuteUpdate(upd) => chat_row_signals(&upd.jid),
        Event::StarUpdate(upd) => chat_row_signals(&upd.chat_jid),
        Event::DeleteMessageForMeUpdate(upd) => chat_row_signals(&upd.chat_jid),
        Event::LabelAssociationUpdate(upd) => chat_row_signals(&upd.chat_jid),
        Event::MessageLabelAssociationUpdate(upd) => chat_row_signals(&upd.chat_jid),
        Event::LabelEditUpdate(_) => vec![UiSignal::Invalidated(Invalidation::Chats)],
        Event::DisappearingModeChanged(_) => {
            vec![UiSignal::Invalidated(Invalidation::Chats)]
        }
        Event::GroupUpdate(_) => {
            // Subject/photo/participant churn reshapes the chat row; member
            // changes that spawn service rows arrive as Messages events.
            vec![UiSignal::Invalidated(Invalidation::Chats)]
        }
        Event::NewsletterLiveUpdate(upd) => {
            vec![UiSignal::Invalidated(scoped(&upd.newsletter_jid))]
        }

        // --- Bulk sync: everything is suspect ----------------------------
        Event::HistorySync(_)
        | Event::OfflineSyncPreview(_)
        | Event::OfflineSyncCompleted(_)
        | Event::DirtyState(_) => vec![UiSignal::Invalidated(Invalidation::Chats)],
        Event::AppStateSyncFailed(report) => {
            // Only buckets that left data missing/stale deserve a sweep;
            // `skipped` changed nothing.
            if report.fatal.is_empty() && report.retryable.is_empty() {
                vec![]
            } else {
                vec![UiSignal::Invalidated(Invalidation::Chats)]
            }
        }

        // --- Deliberately unmapped ---------------------------------------
        // Presence/typing are ephemeral (watch-channel material), crypto and
        // transport plumbing has no UI domain, and calls have no surface yet.
        Event::Notification(_)
        | Event::RawNode(_)
        | Event::DecryptedPayload(_)
        | Event::SentFrame(_)
        | Event::EncDecryptFailed(_)
        | Event::MexNotification(_)
        | Event::DeviceListUpdate(_)
        | Event::IdentityChange(_)
        | Event::CallLogSync(_)
        | Event::ClientExpirationChanged(_)
        | Event::IncomingCall(_)
        | Event::MissedCall(_)
        | Event::CallEndedElsewhere(_)
        | Event::ChatPresence(_)
        | Event::Presence(_)
        | Event::QuickReplyUpdate(_)
        | Event::DisableLinkPreviewsUpdate(_)
        | Event::RetiredPushNameUpdate(_)

        // Rotation continues after a rejected pair attempt; the next QR
        // refreshes the screen. Passkey-driven linking gets its own UX later.
        | Event::PairError(_)
        | Event::PairPasskeyRequest(_)
        | Event::PairPasskeyConfirmation(_)
        | Event::PairPasskeyError(_) => vec![],

        // `Event` is #[non_exhaustive]: future variants degrade to a no-op.
        _ => vec![],
    }
}

/// `Contacts` invalidation, spelled once.
fn contacts() -> UiSignal {
    UiSignal::Invalidated(Invalidation::Contacts)
}

/// A chat-metadata mutation: the chat list row changed, and the message-set
/// projection scoped to that chat is dirty too (read/star/delete flags live
/// on rows there). Over-invalidation is the cheap direction.
fn chat_row_signals(chat: &Jid) -> Vec<UiSignal> {
    vec![
        UiSignal::Invalidated(Invalidation::Chats),
        UiSignal::Invalidated(scoped(chat)),
    ]
}

/// Scope an invalidation to one chat, falling back to the coarse `Chats`
/// sweep when the JID never parsed (empty user on a defaulted `Jid`) —
/// callers cannot key a projection to a chat that has no address.
fn scoped(chat: &Jid) -> Invalidation {
    match chat_of(chat) {
        Some(chat) => Invalidation::Messages { chat },
        None => Invalidation::Chats,
    }
}

/// The chat key as stored text, or `None` for a defaulted (unparsed) JID.
fn chat_of(jid: &Jid) -> Option<String> {
    (!jid.user.is_empty()).then(|| jid.to_string())
}

/// Pseudonymize a JID for logs: `pn#<8 hex>` via an unkeyed `DefaultHasher`.
///
/// A display token, not an identity: stable within a process run so traces
/// correlate, small enough (32 bits) that it must never gate logic. This
/// module itself adds no tracing — it is exported for the delivery layers
/// around it, which do.
#[must_use]
pub fn redact_jid(jid: &str) -> String {
    let mut hasher = DefaultHasher::new();
    jid.hash(&mut hasher);
    format!("pn#{:08x}", hasher.finish() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use whatsapp_rust::chrono::Utc;
    use whatsapp_rust::types::events::{
        BatchOrigin, ClientOutdated, ConnectFailure, ConnectFailureReason, ContactUpdated,
        InboundMessage, LoggedOut, MessageBatch, PairError, PairPasskeyRequest, PairingQrCode,
        Receipt, ServerAck, StarUpdate, TempBanReason, TemporaryBan,
    };
    use whatsapp_rust::types::message::{MessageInfo, MessageSource};
    use whatsapp_rust::types::presence::ReceiptType;
    use whatsapp_rust::waproto::whatsapp as wa;

    /// Fictitious addresses throughout; never production captures.
    const PEER_A: &str = "15550000001";
    const PEER_B: &str = "15550000002";

    fn inbound(chat: &str) -> InboundMessage {
        InboundMessage::builder()
            .message(Arc::new(wa::Message::default()))
            .info(Arc::new(MessageInfo {
                source: MessageSource {
                    chat: Jid::pn(chat),
                    ..Default::default()
                },
                ..Default::default()
            }))
            .build()
    }

    #[test]
    fn messages_batch_dedupes_per_chat() {
        let batch = MessageBatch::builder()
            .messages(Arc::from(vec![
                inbound(PEER_A),
                inbound(PEER_B),
                inbound(PEER_A),
            ]))
            .origin(BatchOrigin::OfflineDrain)
            // Committed rows are visible rows: the invalidation stands.
            .hook_committed(true)
            .build();
        assert_eq!(
            classify(&Event::Messages(batch)),
            vec![
                UiSignal::Invalidated(Invalidation::Messages {
                    chat: Jid::pn(PEER_A).to_string()
                }),
                UiSignal::Invalidated(Invalidation::Messages {
                    chat: Jid::pn(PEER_B).to_string()
                }),
            ]
        );
    }

    #[test]
    fn receipt_scopes_to_chat_when_parseable() {
        let receipt = Receipt::builder()
            .message_ids(vec!["FAKEMSGID1".to_string()])
            .source(MessageSource {
                chat: Jid::pn(PEER_A),
                ..Default::default()
            })
            .timestamp(Utc::now())
            .r#type(ReceiptType::Delivered)
            .offline(false)
            .build();
        assert_eq!(
            classify(&Event::Receipt(receipt)),
            vec![UiSignal::Invalidated(Invalidation::Messages {
                chat: Jid::pn(PEER_A).to_string()
            })]
        );
    }

    #[test]
    fn receipt_with_unparseable_chat_falls_back_to_chats() {
        // Defaulted MessageSource: the receipt parser produced no chat.
        let receipt = Receipt::builder()
            .message_ids(vec!["FAKEMSGID1".to_string()])
            .source(MessageSource::default())
            .timestamp(Utc::now())
            .r#type(ReceiptType::Delivered)
            .offline(false)
            .build();
        assert_eq!(
            classify(&Event::Receipt(receipt)),
            vec![UiSignal::Invalidated(Invalidation::Chats)]
        );
    }

    #[test]
    fn server_ack_filters_by_class_and_chat() {
        let ack = |class: Option<&str>, from: Option<Jid>| {
            ServerAck::builder()
                .id("FAKEMSGID1".to_string())
                .maybe_class(class.map(str::to_string))
                .maybe_from(from)
                .maybe_timestamp(None)
                .maybe_error(None)
                .build()
        };
        // Non-message classes say nothing about chat rows.
        assert!(classify(&Event::ServerAck(ack(Some("receipt"), None))).is_empty());
        // Message ack without a parseable chat: coarse sweep.
        assert_eq!(
            classify(&Event::ServerAck(ack(Some("message"), None))),
            vec![UiSignal::Invalidated(Invalidation::Chats)]
        );
        // Message ack with the chat echoed: scoped.
        assert_eq!(
            classify(&Event::ServerAck(ack(
                Some("message"),
                Some(Jid::pn(PEER_A))
            ))),
            vec![UiSignal::Invalidated(Invalidation::Messages {
                chat: Jid::pn(PEER_A).to_string()
            })]
        );
    }

    #[test]
    fn qr_event_yields_arrival_with_timeout_and_no_payload() {
        let event = Event::PairingQrCode(
            PairingQrCode::builder()
                .code("fake-qr-payload-do-not-render".to_string())
                .timeout(Duration::from_secs(20))
                .build(),
        );
        assert_eq!(
            classify(&event),
            vec![UiSignal::QrArrived {
                expires_in: Duration::from_secs(20)
            }]
        );
        // The secret payload must not leak through Debug of the signals.
        let debugged = format!("{:?}", classify(&event));
        assert!(!debugged.contains("fake-qr-payload"));
    }

    #[test]
    fn contact_update_invalidates_contacts() {
        let updated = ContactUpdated::builder()
            .jid(Jid::pn(PEER_A))
            .timestamp(Utc::now())
            .build();
        assert_eq!(classify(&Event::ContactUpdated(updated)), vec![contacts()]);
    }

    #[test]
    fn star_update_invalidates_chat_and_its_messages() {
        let starred = StarUpdate::builder()
            .chat_jid(Jid::group("1000000000000000001"))
            .maybe_participant_jid(None)
            .message_id("FAKEMSGID1".to_string())
            .from_me(false)
            .timestamp(Utc::now())
            .action(Box::default())
            .from_full_sync(false)
            .build();
        assert_eq!(
            classify(&Event::StarUpdate(starred)),
            vec![
                UiSignal::Invalidated(Invalidation::Chats),
                UiSignal::Invalidated(Invalidation::Messages {
                    chat: Jid::group("1000000000000000001").to_string()
                }),
            ]
        );
    }

    #[test]
    fn logged_out_proposes_failed_state() {
        let logged_out = LoggedOut::builder()
            .on_connect(false)
            .reason(ConnectFailureReason::LoggedOut)
            .maybe_logout_message(None)
            .maybe_raw(None)
            .build();
        assert_eq!(
            classify(&Event::LoggedOut(logged_out)),
            vec![UiSignal::State(SessionState::Failed {
                reason: failure_reason::LOGGED_OUT.to_string()
            })]
        );
    }

    #[test]
    fn forced_logout_is_distinct_from_logged_out() {
        let forced = LoggedOut::builder()
            .on_connect(false)
            .reason(ConnectFailureReason::Generic)
            .maybe_logout_message(None)
            .maybe_raw(None)
            .build();
        assert!(!forced.reason.is_logged_out());
        assert_eq!(
            classify(&Event::LoggedOut(forced)),
            vec![UiSignal::State(SessionState::Failed {
                reason: failure_reason::FORCED_LOGOUT.to_string()
            })]
        );
    }

    #[test]
    fn client_outdated_proposes_failed_state() {
        let outdated = ClientOutdated::builder().maybe_raw(None).build();
        assert_eq!(
            classify(&Event::ClientOutdated(outdated)),
            vec![UiSignal::State(SessionState::Failed {
                reason: failure_reason::CLIENT_OUTDATED.to_string()
            })]
        );
    }

    fn connect_failure(reason: ConnectFailureReason) -> Event {
        Event::ConnectFailure(
            ConnectFailure::builder()
                .reason(reason)
                .maybe_message(None)
                .maybe_raw(None)
                .build(),
        )
    }

    fn failed_reason(event: &Event) -> String {
        match classify(event).as_slice() {
            [UiSignal::State(SessionState::Failed { reason })] => reason.clone(),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn reconnectable_connect_failure_proposes_reconnecting() {
        assert_eq!(
            classify(&connect_failure(ConnectFailureReason::ServiceUnavailable)),
            vec![UiSignal::State(SessionState::Reconnecting)]
        );
    }

    #[test]
    fn rate_limited_connect_failure_uses_stable_label() {
        let reason = failed_reason(&connect_failure(ConnectFailureReason::Unknown(429)));
        assert_eq!(reason, failure_reason::RATE_LIMITED);
        assert!(!reason.contains("429"));
        assert!(!reason.contains("Unknown"));
    }

    #[test]
    fn connect_failure_reason_is_not_debug_dumped() {
        let reason = failed_reason(&connect_failure(ConnectFailureReason::Generic));
        assert_eq!(reason, failure_reason::CONNECT_FAILURE);
        assert!(!reason.contains("Generic"));
        assert!(!reason.contains("ConnectFailureReason"));
        let dumped = format!(
            "{:?}",
            classify(&connect_failure(ConnectFailureReason::NotFound))
        );
        assert!(!dumped.contains("NotFound"));
        assert!(!dumped.contains("ConnectFailureReason"));
    }

    #[test]
    fn temp_banned_connect_failure_has_no_invented_wait() {
        let reason = failed_reason(&connect_failure(ConnectFailureReason::TempBanned));
        assert_eq!(reason, failure_reason::TEMPORARILY_BANNED);
        assert_eq!(failure_reason::temporary_ban_wait_secs(&reason), None);
    }

    #[test]
    fn temporary_ban_encodes_wait_without_dumping_internal_reason() {
        let ban = TemporaryBan::builder()
            .code(TempBanReason::SentToTooManyPeople)
            .expire(whatsapp_rust::chrono::Duration::seconds(3600))
            .maybe_message(None)
            .maybe_url(None)
            .maybe_raw(None)
            .build();
        let reason = failed_reason(&Event::TemporaryBan(ban));
        assert_eq!(reason, "temporarily banned: 3600");
        assert_eq!(failure_reason::temporary_ban_wait_secs(&reason), Some(3600));
        assert!(!reason.contains("address books"));
        assert!(!reason.contains(PEER_A));
        assert!(!reason.contains('@'));
    }

    #[test]
    fn temporary_ban_zero_expire_does_not_invent_a_wait_window() {
        let ban = TemporaryBan::builder()
            .code(TempBanReason::BlockedByUsers)
            .expire(whatsapp_rust::chrono::Duration::seconds(0))
            .maybe_message(None)
            .maybe_url(None)
            .maybe_raw(None)
            .build();
        let reason = failed_reason(&Event::TemporaryBan(ban));
        assert_eq!(reason, failure_reason::TEMPORARILY_BANNED);
        assert_eq!(failure_reason::temporary_ban_wait_secs(&reason), None);
    }

    #[test]
    fn unmapped_events_degrade_to_no_signals() {
        // Pairing-rejection and passkey plumbing stand in for every
        // deliberately-unmapped family: no signals, no panic. Truly unknown
        // future variants cannot be fabricated outside the sealed enum; the
        // `_` catch-all gives them this same no-op.
        let pair_error = PairError::builder()
            .id(Jid::pn(PEER_A))
            .lid(Jid::lid(PEER_A))
            .business_name(String::new())
            .platform(String::new())
            .error("rejected".to_string())
            .build();
        assert_eq!(classify(&Event::PairError(pair_error)), vec![]);
        let passkey = PairPasskeyRequest::builder()
            .request_options_json("{\"fake\":true}".to_string())
            .build();
        assert_eq!(classify(&Event::PairPasskeyRequest(passkey)), vec![]);
    }

    #[test]
    fn debug_output_redacts_chat_jids() {
        let signals = chat_row_signals(&Jid::pn(PEER_A));
        let debugged = format!("{signals:?}");
        assert!(!debugged.contains(PEER_A));
        // The scoped invalidation carries the Jid's Display form, so the
        // redaction token is derived from that same form.
        assert!(debugged.contains(&redact_jid(&Jid::pn(PEER_A).to_string())));
    }

    #[test]
    fn redact_jid_is_stable_hash_form() {
        let a = redact_jid(PEER_A);
        assert_eq!(a.len(), "pn#".len() + 8);
        assert!(a.starts_with("pn#"));
        assert!(a[3..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, redact_jid(PEER_A), "stable within a run");
        assert_ne!(a, redact_jid(PEER_B), "distinct inputs diverge");
    }
}

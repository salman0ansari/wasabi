//! Deterministic charter-grade dataset generator for the account store.
//!
//! Streams the fixture through the store's real write APIs —
//! [`whatsapp_rust_chat_store::ChatStore::apply_inbound`] for incoming
//! traffic (one committed transaction per 500-row batch) and
//! `record_outgoing_async` for the outgoing slice — so at most one batch of
//! fixtures is ever resident; the million-row dataset exists only inside
//! SQLite.
//!
//! Throughput expectation (order-of-magnitude, not a guarantee): with the
//! production tuning (`synchronous=FULL`) every inbound batch pays one fsync,
//! so generation lands around 20-80k rows/s on a modern NVMe drive and one
//! million rows take well under two minutes; the outgoing slice amortizes to
//! one fsync per <=128-row writer-batch drain.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::{DateTime, Utc};
use wacore::proto_helpers::MessageBuilderExt;
use wacore::types::events::InboundMessage;
use wacore::types::message::{MessageInfo, MessageSource};
use wacore_binary::Jid;
use waproto::buffa::MessageField;
use waproto::whatsapp as wa;
use wasabi_repository::AccountStore;

/// Charter-grade scale: 10_000 chats averaging ~100 messages each.
pub const CHATS: u64 = 10_000;
/// Every 7th chat is a group whose senders cycle a fixed participant pool.
const GROUP_EVERY: u64 = 7;
const GROUP_PARTICIPANTS: u64 = 20;
/// apply_inbound commits one transaction per call; 500 rows amortizes the
/// fsync while staying far below SQLite's per-statement limits.
const INBOUND_BATCH: usize = 500;
const PROGRESS_EVERY: u64 = 50_000;

/// Fixed base instant (2023-01-01T00:00:00Z) so identical seeds reproduce
/// identical databases row for row.
const BASE_MS: i64 = 1_672_531_200_000;
/// The whole dataset spans three years of wall-clock time.
const SPAN_MS: i64 = 3 * 365 * 24 * 60 * 60 * 1000;

/// Term embedded in every generated body; bench mode searches it to stress
/// FTS ranking over a large match set instead of a rare-token lookup.
pub const SEARCH_TERM: &str = "update";

pub struct GenReport {
    pub rows: u64,
    pub elapsed: Duration,
}

enum Kind {
    IncomingText,
    OutgoingText,
    ImageMeta,
}

fn kind_for(draw: u64) -> Kind {
    // 70% incoming text, 15% outgoing, 10% image-kind metadata. The last 5%
    // would be reactions-bearing messages, but reactions live in their own
    // tables which this generator deliberately does not populate (outbox and
    // inbound coverage only), so that slice folds into plain incoming text.
    match draw % 100 {
        0..=69 => Kind::IncomingText,
        70..=84 => Kind::OutgoingText,
        85..=94 => Kind::ImageMeta,
        _ => Kind::IncomingText,
    }
}

/// Tiny LCG; only the high bits feed the mix buckets because an LCG's low
/// bits have short periods.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn is_group_chat(c: u64) -> bool {
    c % GROUP_EVERY == GROUP_EVERY - 1
}

fn chat_jid(c: u64) -> String {
    if is_group_chat(c) {
        format!("120363011{c:05}@g.us")
    } else {
        format!("5599{:08}@s.whatsapp.net", 30_000_000 + c)
    }
}

fn participant_jid(p: u64) -> String {
    format!("5597{:08}@s.whatsapp.net", 41_000_000 + p)
}

fn jid(s: &str) -> anyhow::Result<Jid> {
    s.parse()
        .map_err(|e| anyhow::anyhow!("generated jid {s} failed to parse: {e}"))
}

fn ms_to_dt(ms: i64) -> anyhow::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(ms).context("generated timestamp out of range")
}

const PHRASES: [&str; 8] = [
    "standup notes",
    "lunch plans",
    "photo from the trip",
    "invoice attached",
    "running late",
    "call me back",
    "schedule moved",
    "check this link",
];

fn body(ordinal: u64) -> String {
    format!(
        "{} {} #{}",
        PHRASES[(ordinal as usize) % PHRASES.len()],
        SEARCH_TERM,
        ordinal
    )
}

fn image_message(ordinal: u64) -> wa::Message {
    // Metadata-only media payload: url/mime/dimensions are enough for the
    // store to classify an image bubble; no bytes exist behind this bench.
    wa::Message {
        image_message: MessageField::some(wa::message::ImageMessage {
            url: Some(format!("https://media.gen.invalid/{ordinal}.jpg")),
            mimetype: Some("image/jpeg".into()),
            file_length: Some(48_000 + ordinal % 4_096),
            width: Some(1080),
            height: Some(1350),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn inbound_fixture(
    chat: &Jid,
    sender: &Jid,
    is_group: bool,
    id: &str,
    ts: DateTime<Utc>,
    push_name: &str,
    msg: wa::Message,
) -> InboundMessage {
    let info = MessageInfo {
        source: MessageSource {
            chat: chat.clone(),
            sender: sender.clone(),
            is_from_me: false,
            is_group,
            ..Default::default()
        },
        id: id.to_string(),
        timestamp: ts,
        push_name: push_name.to_string(),
        ..Default::default()
    };
    InboundMessage::builder()
        .message(Arc::new(msg))
        .info(Arc::new(info))
        .build()
}

/// Generate `total_rows` messages across [`CHATS`] chats into `store`'s
/// database. Timestamps ascend globally (three-year span), so every chat's
/// history orders naturally and later chats are newer.
pub async fn generate(store: &AccountStore, total_rows: u64) -> anyhow::Result<GenReport> {
    let started = Instant::now();
    let chats = store.chats().clone();
    let mut rng = Lcg::new(0xDA7A_BA5E);
    // Spacing floors at 1ms so timestamps stay strictly ascending even for
    // row counts larger than the span.
    let spacing_ms = (SPAN_MS / total_rows.max(1) as i64).max(1);
    let per_chat = total_rows / CHATS;
    let remainder = total_rows % CHATS;

    let mut batch: Vec<InboundMessage> = Vec::with_capacity(INBOUND_BATCH);
    let mut ordinal: u64 = 0;

    for c in 0..CHATS {
        let count = per_chat + u64::from(c < remainder);
        if count == 0 {
            continue;
        }
        let chat = jid(&chat_jid(c))?;
        let group = is_group_chat(c);
        for _ in 0..count {
            let ts = ms_to_dt(BASE_MS + ordinal as i64 * spacing_ms)?;
            let id = format!("GEN{ordinal:09}");
            // Group traffic rotates a fixed sender pool so bubbles carry
            // distinct participants without unbounded contact churn.
            let sender = if group {
                jid(&participant_jid(ordinal % GROUP_PARTICIPANTS))?
            } else {
                chat.clone()
            };
            match kind_for(rng.next()) {
                Kind::Outgoing => {
                    chats
                        .record_outgoing_async(&chat, id, &wa::Message::text(body(ordinal)), ts)
                        .await?;
                }
                kind => {
                    let msg = match kind {
                        Kind::ImageMeta => image_message(ordinal),
                        _ => wa::Message::text(body(ordinal)),
                    };
                    let push = if group {
                        format!("GenUser{:02}", ordinal % GROUP_PARTICIPANTS)
                    } else {
                        String::new()
                    };
                    batch.push(inbound_fixture(
                        &chat, &sender, group, &id, ts, &push, msg,
                    ));
                    if batch.len() >= INBOUND_BATCH {
                        // Swap in a fresh capacity-carrying buffer so the
                        // committed one moves out without realloc churn.
                        let full = std::mem::replace(
                            &mut batch,
                            Vec::with_capacity(INBOUND_BATCH),
                        );
                        chats.apply_inbound(full).await?;
                    }
                }
            }
            ordinal += 1;
            if ordinal % PROGRESS_EVERY == 0 {
                // Periodic drain keeps the bounded writer queue shallow over
                // long runs instead of relying purely on backpressure.
                store.flush().await?;
                let elapsed = started.elapsed().as_secs_f64();
                tracing::info!(
                    rows = ordinal,
                    elapsed_s = elapsed,
                    rate_rows_s = (ordinal as f64 / elapsed) as u64,
                    "generation progress"
                );
            }
        }
    }
    if !batch.is_empty() {
        chats.apply_inbound(batch).await?;
    }
    // Drain the write-behind queue so every outgoing row is on disk before
    // the report prints.
    store.flush().await?;
    Ok(GenReport {
        rows: ordinal,
        elapsed: started.elapsed(),
    })
}

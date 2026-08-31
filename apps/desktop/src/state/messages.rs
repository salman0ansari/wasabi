//! Bounded sliding-window message model.
//!
//! At most [`WINDOW_MAX`] rows live in memory, anchored at the newest end on
//! selection. Scrolling up prepends keyset pages and trims the tail once the
//! window overflows. Render order (date separators interleaved) and row
//! stable item identities are prepared eagerly so GPUI can measure actual
//! rendered heights without losing the scroll anchor.

use std::collections::HashSet;

use wasabi_domain::{
    ChatSummary, MessageContext, MessageDirection, MessageKind, MessagePage, MessageRow,
    MessageStatus,
};

use crate::state::chats::{fallback_name, is_group};

/// Hard cap of the in-memory window around the viewport.
pub const WINDOW_MAX: usize = 200;
/// Prepared render order: date chips between day groups, messages by index
/// into [`MessageWindowModel::rows`].
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineItem {
    Date(String),
    Message(usize),
}

/// Stable identity parallel to [`TimelineItem`]. Row indices are intentionally
/// excluded so a prepend can preserve the exact visible message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineKey {
    Date(String),
    Message(wasabi_domain::MessageId, i64),
}

#[derive(Default)]
pub struct MessageWindowModel {
    pub chat_id: Option<String>,
    /// Oldest first; the store pages newest-first and is reversed here.
    pub rows: Vec<MessageRow>,
    pub items: Vec<TimelineItem>,
    pub has_more_older: bool,
    /// True once the newest tail was trimmed to keep the window bounded.
    #[allow(dead_code)]
    pub has_more_newer: bool,
    pub loading: bool,
    pub loading_older: bool,
    pub loading_newer: bool,
    pub error: Option<String>,
    /// Search/action target receiving a temporary accent outline.
    pub highlighted: Option<wasabi_domain::MessageId>,
}

impl MessageWindowModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything and prepare for a fresh anchored load.
    pub fn reset_for_chat(&mut self, chat_id: &str) {
        *self = Self::new();
        self.chat_id = Some(chat_id.to_string());
        self.loading = true;
    }

    /// Anchor at the newest end: keep only the newest [`WINDOW_MAX`] rows.
    pub fn anchor_newest(&mut self, page: &MessagePage) {
        let mut rows = page.rows.clone();
        rows.reverse();
        if rows.len() > WINDOW_MAX {
            let drop = rows.len() - WINDOW_MAX;
            rows.drain(..drop);
            self.has_more_older = true;
        } else {
            self.has_more_older = page.next_before.is_some();
        }
        self.rows = rows;
        self.has_more_newer = false;
        self.loading = false;
        self.error = None;
        self.highlighted = None;
        self.rebuild();
    }

    /// Replace the window with bounded context around an exact target.
    pub fn anchor_context(&mut self, context: &MessageContext) {
        let mut rows = context.rows.clone();
        rows.reverse();
        self.rows = rows;
        self.has_more_older = context.has_more_older;
        self.has_more_newer = context.has_more_newer;
        self.loading = false;
        self.loading_older = false;
        self.loading_newer = false;
        self.error = None;
        self.highlighted = Some(context.anchor.clone());
        self.rebuild();
    }

    /// Render-list index (including date chips) for an exact message.
    pub fn timeline_index_for_message(&self, message: &wasabi_domain::MessageId) -> Option<usize> {
        self.items.iter().position(|item| match item {
            TimelineItem::Message(row) => self
                .rows
                .get(*row)
                .is_some_and(|candidate| &candidate.id == message),
            TimelineItem::Date(_) => false,
        })
    }

    pub fn newer_anchor(&self) -> Option<wasabi_domain::MessageId> {
        self.rows.last().map(|row| row.id.clone())
    }

    /// Append the next newer anchored page while preserving the current
    /// viewport. Returns the number of genuinely new rows added.
    pub fn append_newer_context(&mut self, context: &MessageContext) -> usize {
        self.loading_newer = false;
        let mut seen = self.rows.iter().map(row_key).collect::<HashSet<_>>();
        let mut newer = context
            .rows
            .iter()
            .rev()
            .filter(|row| seen.insert(row_key(row)))
            .cloned()
            .collect::<Vec<_>>();
        let added = newer.len();
        self.rows.append(&mut newer);
        self.rows.sort_by_key(|row| (row.timestamp_ms, row.seq.0));
        let overflow = self.rows.len().saturating_sub(WINDOW_MAX);
        if overflow > 0 {
            self.rows.drain(..overflow);
            self.has_more_older = true;
        }
        self.has_more_newer = context.has_more_newer;
        if self
            .highlighted
            .as_ref()
            .is_some_and(|target| !self.rows.iter().any(|row| &row.id == target))
        {
            self.highlighted = None;
        }
        self.rebuild();
        added
    }

    /// Prepend an older page, trimming the newest tail when the window would
    /// exceed its cap. Returns how many items were added at the front so the
    /// view can re-anchor scrolling.
    pub fn prepend_older(&mut self, page: &MessagePage) -> usize {
        self.loading_older = false;
        let existing = self.rows.iter().map(row_key).collect::<HashSet<_>>();
        let mut seen = existing;
        let mut older = page
            .rows
            .iter()
            .rev()
            .filter(|row| seen.insert(row_key(row)))
            .cloned()
            .collect::<Vec<_>>();
        let added = older.len();
        if added == 0 {
            self.has_more_older = page.next_before.is_some();
            return 0;
        }
        let mut rows = std::mem::take(&mut older);
        rows.append(&mut self.rows);

        let overflow = rows.len().saturating_sub(WINDOW_MAX);
        if overflow > 0 {
            // Dropping from the tail keeps the region the user is reading
            // intact; the newest end reloads cheaply on any invalidation.
            rows.truncate(WINDOW_MAX);
            self.has_more_newer = true;
        }
        self.rows = rows;
        self.has_more_older = page.next_before.is_some();
        self.rebuild();
        added
    }

    /// Append newer rows without evicting the history currently being read.
    /// Returns the total unseen count; rows beyond the bounded window remain
    /// available through an anchored newest reload.
    pub fn merge_newer(&mut self, page: &MessagePage) -> usize {
        // A refresh page can overlap the current window at any position, not
        // only at its last row. Merge by stable identity before sorting so a
        // mid-history refresh never creates duplicate bubbles.
        let mut seen = self.rows.iter().map(row_key).collect::<HashSet<_>>();
        let newest = self.rows.last().map(|row| (row.timestamp_ms, row.seq.0));
        let newer = page
            .rows
            .iter()
            .rev()
            .filter(|row| {
                newest.is_none_or(|newest| (row.timestamp_ms, row.seq.0) > newest)
                    && seen.insert(row_key(row))
            })
            .cloned()
            .collect::<Vec<_>>();
        let unseen = newer.len();
        let available = WINDOW_MAX.saturating_sub(self.rows.len());
        self.rows.extend(newer.into_iter().take(available));
        self.rows.sort_by_key(|row| (row.timestamp_ms, row.seq.0));
        self.has_more_newer = unseen > available;
        self.rebuild();
        unseen
    }

    pub fn set_error(&mut self, message: String) {
        self.loading = false;
        self.loading_older = false;
        self.error = Some(message);
    }

    /// Cursor to fetch the page before the oldest loaded row.
    pub fn older_cursor(&self) -> Option<wasabi_domain::PageCursor> {
        let first = self.rows.first()?;
        Some(wasabi_domain::PageCursor {
            timestamp_ms: first.timestamp_ms,
            seq: first.seq,
        })
    }

    /// Recompute the render order. Actual heights are measured and cached by
    /// GPUI's native variable-height list.
    pub fn rebuild(&mut self) {
        use chrono::TimeZone;

        let mut items = Vec::with_capacity(self.rows.len() + 8);
        let now_local = chrono::Local::now().date_naive();

        let mut prev_day: Option<chrono::NaiveDate> = None;
        for (ix, row) in self.rows.iter().enumerate() {
            let day = chrono::Local
                .timestamp_millis_opt(row.timestamp_ms)
                .single()
                .map(|t| t.date_naive());
            if day != prev_day {
                prev_day = day;
                let label = match day {
                    Some(d) => chip_label(d, now_local),
                    None => String::new(),
                };
                items.push(TimelineItem::Date(label));
            }
            items.push(TimelineItem::Message(ix));
        }
        self.items = items;
    }

    pub fn timeline_keys(&self) -> Vec<TimelineKey> {
        self.items
            .iter()
            .filter_map(|item| match item {
                TimelineItem::Date(label) => Some(TimelineKey::Date(label.clone())),
                TimelineItem::Message(index) => self
                    .rows
                    .get(*index)
                    .map(|row| TimelineKey::Message(row.id.clone(), row.seq.0)),
            })
            .collect()
    }
}

/// Stable UI identity for a row. Message ids are only unique within a
/// sender/chat in the protocol, so the sequence tiebreak remains part of the
/// projection key when pages overlap.
fn row_key(row: &MessageRow) -> (wasabi_domain::MessageId, i64) {
    (row.id.clone(), row.seq.0)
}

/// Day-chip label relative to today.
fn chip_label(day: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    if day == today {
        "Today".to_string()
    } else if day == today - chrono::Duration::days(1) {
        "Yesterday".to_string()
    } else {
        day.format("%d %b %Y").to_string()
    }
}

/// Plain-text projection used for bubbles and previews.
pub fn body_text(row: &MessageRow) -> String {
    match &row.kind {
        MessageKind::Text { body } => body.clone(),
        MessageKind::Image { caption, .. } => caption.clone().unwrap_or_else(|| "Photo".into()),
        MessageKind::Video { caption, .. } => caption.clone().unwrap_or_else(|| "Video".into()),
        MessageKind::Audio { voice_note, .. } => {
            if *voice_note {
                "Voice message".to_string()
            } else {
                "Audio".to_string()
            }
        }
        MessageKind::Document { media } => media
            .file_name
            .clone()
            .unwrap_or_else(|| "Document".to_string()),
        MessageKind::Sticker { .. } => "Sticker".to_string(),
        MessageKind::Location { name, live, .. } => match name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => {
                if *live && name != "Live location" {
                    format!("Live location · {name}")
                } else if !*live && name != "Location" {
                    format!("Location · {name}")
                } else {
                    name.to_string()
                }
            }
            _ => {
                if *live {
                    "Live location".to_string()
                } else {
                    "Location".to_string()
                }
            }
        },
        MessageKind::Contact { display_name, .. } => display_name.clone(),
        MessageKind::Poll { name, quiz, .. } => poll_body_text(name, *quiz),
        MessageKind::Reaction { emoji } => emoji.clone(),
        MessageKind::System { text } => text.clone(),
        MessageKind::Unavailable { reason } => unavailable_text(*reason).to_string(),
        MessageKind::Unknown => "Unsupported message".to_string(),
    }
}

pub fn contact_count_label(contacts: usize) -> String {
    if contacts <= 1 {
        "Contact".to_string()
    } else {
        format!("{contacts} contacts")
    }
}

pub fn poll_kind_label(quiz: bool) -> &'static str {
    if quiz { "Quiz" } else { "Poll" }
}

pub fn poll_body_text(name: &str, quiz: bool) -> String {
    let name = name.trim();
    if name.is_empty() {
        poll_kind_label(quiz).to_string()
    } else {
        name.to_string()
    }
}

pub fn unavailable_text(reason: wasabi_domain::UnavailableMessageReason) -> &'static str {
    use wasabi_domain::UnavailableMessageReason;

    match reason {
        UnavailableMessageReason::WaitingForDecryption => {
            "Waiting for this message. It may take a moment."
        }
        UnavailableMessageReason::ViewOnceOnPhone => "View-once message. Open it on your phone.",
        UnavailableMessageReason::HostedContent => {
            "This hosted message is unavailable on linked devices."
        }
        UnavailableMessageReason::BotContent => "This automated message cannot be displayed yet.",
    }
}

/// Delivery indicator for outgoing rows per the design reference.
pub fn status_glyph(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Pending => "⌛",
        MessageStatus::ServerAck => "✓",
        MessageStatus::Delivered | MessageStatus::Read => "✓✓",
        MessageStatus::Failed => "!",
    }
}

pub fn status_color(status: MessageStatus) -> gpui::Rgba {
    use crate::theme;
    match status {
        MessageStatus::Read => theme::read_receipt(),
        MessageStatus::Failed => theme::danger(),
        _ => theme::text_secondary(),
    }
}

/// Sender display name: push name when resolved, else bare identity. Group
/// conversations always surface it.
pub fn sender_display(row: &MessageRow) -> String {
    row.sender
        .push_name
        .clone()
        .unwrap_or_else(|| row.sender.bare.split('@').next().unwrap_or("").to_string())
}

pub fn sender_is_group_member(row: &MessageRow) -> bool {
    is_group(row.chat.as_str()) && row.direction == MessageDirection::Incoming
}

/// Relative timestamp for list rows: clock time today, weekday within the
/// week, short numeric date otherwise.
pub fn relative_time(ms: i64) -> String {
    use chrono::TimeZone;
    let Some(t) = chrono::Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    let today = chrono::Local::now().date_naive();
    let day = t.date_naive();
    if day == today {
        t.format("%H:%M").to_string()
    } else if day == today - chrono::Duration::days(1) {
        "Yesterday".to_string()
    } else if (today - day).num_days() < 7 {
        t.format("%a").to_string()
    } else {
        t.format("%d/%m/%y").to_string()
    }
}

/// Header subtitle for the selected conversation.
pub fn conversation_subtitle(chat: &ChatSummary) -> String {
    if is_group(chat.id.as_str()) {
        "group".to_string()
    } else {
        format!("last seen {}", relative_time(chat.last_activity_ms))
    }
}

/// Avatar initials from the best available display source.
pub fn avatar_initials(chat: &ChatSummary) -> String {
    fallback_name(chat)
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "#".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasabi_domain::{
        ChatId, LocalCursor, MessageContext, MessageDirection, MessageId, MessageKind,
        MessageStatus, SenderJid,
    };

    #[test]
    fn unavailable_messages_explain_the_correct_recovery_path() {
        use wasabi_domain::UnavailableMessageReason;

        assert!(
            unavailable_text(UnavailableMessageReason::WaitingForDecryption).contains("moment")
        );
        assert!(unavailable_text(UnavailableMessageReason::ViewOnceOnPhone).contains("phone"));
        assert!(
            unavailable_text(UnavailableMessageReason::HostedContent).contains("linked devices")
        );
        assert!(unavailable_text(UnavailableMessageReason::BotContent).contains("automated"));
    }

    #[test]
    fn location_and_contact_body_text_uses_honest_labels() {
        let mut location = row("LOC", 1);
        location.kind = MessageKind::Location {
            name: Some("Harbor Park".to_string()),
            address: Some("12 Waterfront Way".to_string()),
            latitude: Some("37.808".to_string()),
            longitude: Some("-122.4095".to_string()),
            live: false,
        };
        assert_eq!(body_text(&location), "Location · Harbor Park");

        location.kind = MessageKind::Location {
            name: None,
            address: None,
            latitude: None,
            longitude: None,
            live: true,
        };
        assert_eq!(body_text(&location), "Live location");

        location.kind = MessageKind::Contact {
            display_name: "Jordan Blake".to_string(),
            contacts: 1,
        };
        assert_eq!(body_text(&location), "Jordan Blake");
        assert!(!body_text(&location).contains("VCARD"));
        assert_eq!(contact_count_label(1), "Contact");
        assert_eq!(contact_count_label(3), "3 contacts");

        location.kind = MessageKind::Poll {
            name: "Weekend plans?".to_string(),
            options: vec!["Park".to_string(), "Cinema".to_string()],
            selectable_count: 1,
            quiz: false,
        };
        assert_eq!(body_text(&location), "Weekend plans?");
        location.kind = MessageKind::Poll {
            name: String::new(),
            options: Vec::new(),
            selectable_count: 1,
            quiz: true,
        };
        assert_eq!(body_text(&location), "Quiz");
        assert_eq!(poll_kind_label(false), "Poll");
    }

    fn row(id: &str, order: i64) -> MessageRow {
        MessageRow {
            id: MessageId::new(id),
            chat: ChatId::new("chat@s.whatsapp.net"),
            direction: MessageDirection::Incoming,
            sender: SenderJid {
                bare: "peer@s.whatsapp.net".to_string(),
                push_name: None,
            },
            timestamp_ms: order * 1_000,
            seq: LocalCursor(order),
            kind: MessageKind::Text {
                body: id.to_string(),
            },
            quoted: None,
            reactions: Vec::new(),
            status: MessageStatus::Delivered,
            edited_at_ms: None,
            revoked: false,
            starred: false,
        }
    }

    #[test]
    fn anchored_window_centers_and_pages_toward_newest_without_duplicates() {
        let mut model = MessageWindowModel::new();
        model.anchor_context(&MessageContext {
            rows: vec![row("M3", 3), row("M2", 2), row("M1", 1)],
            anchor: MessageId::new("M2"),
            has_more_older: true,
            has_more_newer: true,
        });
        assert_eq!(
            model
                .rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["M1", "M2", "M3"]
        );
        assert!(
            model
                .timeline_index_for_message(&MessageId::new("M2"))
                .is_some()
        );

        let added = model.append_newer_context(&MessageContext {
            rows: vec![row("M5", 5), row("M4", 4), row("M3", 3)],
            anchor: MessageId::new("M3"),
            has_more_older: false,
            has_more_newer: false,
        });
        assert_eq!(added, 2);
        assert_eq!(
            model
                .rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["M1", "M2", "M3", "M4", "M5"]
        );
        assert!(!model.has_more_newer);
    }

    #[test]
    fn incoming_rows_never_evict_history_while_reading() {
        let mut model = MessageWindowModel::new();
        model.anchor_newest(&MessagePage {
            rows: (1..=WINDOW_MAX)
                .rev()
                .map(|order| row(&format!("M{order}"), order as i64))
                .collect(),
            next_before: None,
        });
        let oldest = model.rows.first().unwrap().id.clone();
        let newest = model.rows.last().unwrap().id.clone();

        let unseen = model.merge_newer(&MessagePage {
            rows: vec![row("NEW", WINDOW_MAX as i64 + 1)],
            next_before: None,
        });

        assert_eq!(unseen, 1);
        assert_eq!(model.rows.len(), WINDOW_MAX);
        assert_eq!(model.rows.first().unwrap().id, oldest);
        assert_eq!(model.rows.last().unwrap().id, newest);
        assert!(model.has_more_newer);
    }
}

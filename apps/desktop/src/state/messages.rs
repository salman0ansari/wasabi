//! Bounded sliding-window message model.
//!
//! At most [`WINDOW_MAX`] rows live in memory, anchored at the newest end on
//! selection. Scrolling up prepends keyset pages and trims the tail once the
//! window overflows. Render order (date separators interleaved) and row
//! height estimates are prepared eagerly so `render` stays a pure read.

use std::collections::{HashMap, HashSet};

use wasabi_domain::{
    ChatSummary, MessageDirection, MessageKind, MessagePage, MessageRow, MessageStatus,
};

use crate::state::chats::{fallback_name, is_group};

/// Hard cap of the in-memory window around the viewport.
pub const WINDOW_MAX: usize = 200;
const CHARS_PER_LINE: f32 = 58.0;
const LINE_H: f32 = 20.0;
const BUBBLE_BASE_H: f32 = 46.0;
const MEDIA_BASE_H: f32 = 120.0;

/// Prepared render order: date chips between day groups, messages by index
/// into [`MessageWindowModel::rows`].
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineItem {
    Date(String),
    Message(usize),
}

#[derive(Default)]
pub struct MessageWindowModel {
    pub chat_id: Option<String>,
    /// Oldest first; the store pages newest-first and is reversed here.
    pub rows: Vec<MessageRow>,
    pub items: Vec<TimelineItem>,
    /// Pixel heights parallel to `items`.
    pub sizes: Vec<f32>,
    pub has_more_older: bool,
    /// True once the newest tail was trimmed to keep the window bounded.
    #[allow(dead_code)]
    pub has_more_newer: bool,
    pub loading: bool,
    pub loading_older: bool,
    pub error: Option<String>,
    estimates: HashMap<wasabi_domain::MessageId, f32>,
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
        self.rebuild();
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
        rows.extend(self.rows.drain(..));

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

    /// Append newer rows (invalidation refresh while scrolled up).
    pub fn merge_newer(&mut self, page: &MessagePage) {
        // A refresh page can overlap the current window at any position, not
        // only at its last row. Merge by stable identity before sorting so a
        // mid-history refresh never creates duplicate bubbles.
        let mut seen = self.rows.iter().map(row_key).collect::<HashSet<_>>();
        for row in page.rows.iter().rev() {
            if seen.insert(row_key(row)) {
                self.rows.push(row.clone());
            }
        }
        self.rows.sort_by_key(|row| (row.timestamp_ms, row.seq.0));
        if self.rows.len() > WINDOW_MAX {
            let drop = self.rows.len() - WINDOW_MAX;
            self.rows.drain(..drop);
            self.has_more_older = true;
        }
        self.rebuild();
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

    /// Recompute the render order and heights. Called on every mutation.
    pub fn rebuild(&mut self) {
        use chrono::TimeZone;

        let mut items = Vec::with_capacity(self.rows.len() + 8);
        let mut sizes = Vec::with_capacity(self.rows.len() + 8);
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
                sizes.push(crate::theme::DATE_CHIP_H);
                items.push(TimelineItem::Date(label));
            }
            // Split field borrows: rows read, estimates written in place.
            sizes.push(height_of(&mut self.estimates, row));
            items.push(TimelineItem::Message(ix));
        }
        self.items = items;
        self.sizes = sizes;
    }
}

/// Stable UI identity for a row. Message ids are only unique within a
/// sender/chat in the protocol, so the sequence tiebreak remains part of the
/// projection key when pages overlap.
fn row_key(row: &MessageRow) -> (wasabi_domain::MessageId, i64) {
    (row.id.clone(), row.seq.0)
}

/// Cached chars-per-line height heuristic keyed by message id.
fn height_of(estimates: &mut HashMap<wasabi_domain::MessageId, f32>, row: &MessageRow) -> f32 {
    *estimates.entry(row.id.clone()).or_insert_with(|| {
        let text_len = body_text(row).chars().count() as f32;
        let media = matches!(
            row.kind,
            MessageKind::Image { .. }
                | MessageKind::Video { .. }
                | MessageKind::Sticker { .. }
                | MessageKind::Audio { .. }
                | MessageKind::Document { .. }
        );
        let base = if media { MEDIA_BASE_H } else { BUBBLE_BASE_H };
        base + (text_len / CHARS_PER_LINE).ceil().max(1.0) * LINE_H
    })
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
        MessageKind::Audio { .. } => "Voice message".to_string(),
        MessageKind::Document { file_name, .. } => {
            file_name.clone().unwrap_or_else(|| "Document".to_string())
        }
        MessageKind::Sticker { .. } => "Sticker".to_string(),
        MessageKind::Reaction { emoji } => emoji.clone(),
        MessageKind::System { text } => text.clone(),
        MessageKind::Unknown => "Unsupported message".to_string(),
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
        MessageStatus::Read => theme::ACCENT_TEXT,
        MessageStatus::Failed => theme::DANGER,
        _ => theme::TEXT_SECONDARY,
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

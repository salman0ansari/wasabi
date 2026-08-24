//! Chat list page model: one loaded keyset page plus client-side filtering.

use std::cmp::Ordering;

use wasabi_domain::{ChatPageCursor, ChatSummary};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatFilter {
    #[default]
    All,
    Unread,
    Favorites,
    Groups,
}

impl ChatFilter {
    pub const ALL: [ChatFilter; 4] = [
        ChatFilter::All,
        ChatFilter::Unread,
        ChatFilter::Favorites,
        ChatFilter::Groups,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            ChatFilter::All => "All",
            ChatFilter::Unread => "Unread",
            ChatFilter::Favorites => "Favorites",
            ChatFilter::Groups => "Groups",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatSortMode {
    #[default]
    Recent,
    Name,
}

impl ChatSortMode {
    pub const fn toggle(self) -> Self {
        match self {
            ChatSortMode::Recent => ChatSortMode::Name,
            ChatSortMode::Name => ChatSortMode::Recent,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            ChatSortMode::Recent => "Recent",
            ChatSortMode::Name => "Name",
        }
    }
}

#[derive(Default)]
pub struct ChatListModel {
    pub chats: Vec<ChatSummary>,
    /// Indexes into `chats` surviving filter+query; rebuilt on any change.
    pub visible_cache: Vec<usize>,
    pub loading: bool,
    /// Whether the store reported more pages after the loaded one.
    #[allow(dead_code)]
    pub has_more: bool,
    pub filter: ChatFilter,
    pub query: String,
    pub sort_mode: ChatSortMode,
    pub selected: Option<String>,
    pub error: Option<String>,
}

impl ChatListModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the current page (first page or invalidation refresh).
    pub fn set_page(&mut self, rows: Vec<ChatSummary>, has_more: bool) {
        self.chats = rows;
        self.has_more = has_more;
        self.loading = false;
        self.error = None;
        // Keep the selection only if it still exists in the fresh page.
        if let Some(sel) = &self.selected {
            if !self.chats.iter().any(|c| c.id.as_str() == sel.as_str()) {
                self.selected = None;
            }
        }
    }

    pub fn set_error(&mut self, message: String) {
        self.loading = false;
        self.error = Some(message);
    }

    pub fn toggle_sort(&mut self) {
        self.sort_mode = self.sort_mode.toggle();
        self.visible_cache = self.visible();
    }

    /// Keyset cursor reconstructed from the last row of the loaded page.
    /// Kept for chat-list "load more" paging once the list grows beyond one
    /// page; unused by the first shell.
    #[allow(dead_code)]
    pub fn next_cursor(&self) -> Option<ChatPageCursor> {
        let last = self.chats.last()?;
        Some(ChatPageCursor {
            pinned_at_ms: last.pinned_at_ms,
            last_activity_ms: last.last_activity_ms,
            chat: last.id.clone(),
        })
    }

    /// Indexes into `chats` of rows surviving the active filter chip and
    /// search query, ordered by the active sort mode.
    pub fn visible(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        let mut visible = self
            .chats
            .iter()
            .enumerate()
            .filter(|(_, c)| matches_filter(c, self.filter, &query))
            .map(|(ix, _)| ix)
            .collect::<Vec<_>>();
        visible.sort_by(|left, right| {
            compare_chats(&self.chats[*left], &self.chats[*right], self.sort_mode)
        });
        visible
    }
}

fn compare_chats(left: &ChatSummary, right: &ChatSummary, mode: ChatSortMode) -> Ordering {
    match mode {
        ChatSortMode::Recent => right
            .pinned_at_ms
            .cmp(&left.pinned_at_ms)
            .then_with(|| right.last_activity_ms.cmp(&left.last_activity_ms))
            .then_with(|| left.id.as_str().cmp(right.id.as_str())),
        ChatSortMode::Name => {
            let left_name = fallback_name(left);
            let right_name = fallback_name(right);
            left_name
                .to_lowercase()
                .cmp(&right_name.to_lowercase())
                .then_with(|| left_name.cmp(&right_name))
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        }
    }
}

/// WhatsApp group JIDs carry the `@g.us` host; everything else is a direct
/// conversation or broadcast-style thread.
pub fn is_group(chat_jid: &str) -> bool {
    chat_jid.ends_with("@g.us")
}

fn matches_filter(chat: &ChatSummary, filter: ChatFilter, query: &str) -> bool {
    let passes_chip = match filter {
        ChatFilter::All => true,
        ChatFilter::Unread => chat.unread_count != 0,
        // The store exposes no "starred" aggregate on summaries yet; the
        // pinned flag is the closest durable favorite marker.
        ChatFilter::Favorites => chat.pinned_at_ms.is_some(),
        ChatFilter::Groups => is_group(chat.id.as_str()),
    };
    let matches_query = query.is_empty()
        || chat
            .display_name
            .as_ref()
            .is_some_and(|n| n.to_lowercase().contains(query))
        || chat
            .last_message_preview
            .as_ref()
            .is_some_and(|p| p.to_lowercase().contains(query))
        || fallback_name(chat).to_lowercase().contains(query);
    passes_chip && matches_query
}

/// Display name when the store has none resolved: the bare identity part of
/// the chat id.
pub fn fallback_name(chat: &ChatSummary) -> String {
    let raw = chat
        .display_name
        .clone()
        .unwrap_or_else(|| chat.id.as_str().to_string());
    raw.split('@').next().unwrap_or(&raw).to_string()
}

#[cfg(test)]
mod tests {
    use wasabi_domain::{ChatId, ChatSummary};

    use super::{ChatFilter, ChatListModel, ChatSortMode};

    fn chat(
        id: &str,
        name: Option<&str>,
        last_activity_ms: i64,
        unread_count: i64,
        pinned_at_ms: Option<i64>,
    ) -> ChatSummary {
        ChatSummary {
            id: ChatId::new(id),
            display_name: name.map(str::to_string),
            last_activity_ms,
            last_message_preview: None,
            unread_count,
            pinned_at_ms,
            muted_until_ms: None,
            archived: false,
        }
    }

    #[test]
    fn visible_uses_deterministic_recent_and_name_orders() {
        let mut model = ChatListModel::new();
        model.set_page(
            vec![
                chat("z@c.us", Some("Zed"), 300, 0, None),
                chat("a@c.us", Some("Alice"), 100, 0, None),
                chat("p@c.us", Some("Pinned"), 1, 0, Some(10)),
                chat("b@c.us", Some("alice"), 200, 0, None),
            ],
            false,
        );

        assert_eq!(model.visible(), vec![2, 0, 3, 1]);

        model.toggle_sort();
        assert_eq!(model.sort_mode, ChatSortMode::Name);
        assert_eq!(model.visible(), vec![1, 3, 2, 0]);

        model.toggle_sort();
        assert_eq!(model.sort_mode, ChatSortMode::Recent);
        assert_eq!(model.visible_cache, vec![2, 0, 3, 1]);
    }

    #[test]
    fn sorting_happens_after_filter_and_query_without_losing_selection() {
        let mut model = ChatListModel::new();
        model.set_page(
            vec![
                chat("group-a@g.us", Some("Zeta group"), 100, 1, None),
                chat("direct-a@c.us", Some("Alice"), 300, 0, None),
                chat("group-b@g.us", Some("Alpha group"), 200, 2, None),
            ],
            false,
        );
        model.selected = Some("group-a@g.us".to_string());
        model.filter = ChatFilter::Groups;
        model.query = "group".to_string();

        assert_eq!(model.visible(), vec![2, 0]);

        model.toggle_sort();
        assert_eq!(model.visible_cache, vec![2, 0]);
        assert_eq!(model.selected.as_deref(), Some("group-a@g.us"));
        assert_eq!(model.visible(), vec![2, 0]);
    }
}

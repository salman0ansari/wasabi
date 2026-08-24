//! Chat list page model: one loaded keyset page plus client-side filtering.

use wasabi_domain::{ChatPage, ChatPageCursor, ChatScope, ChatSummary};

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

#[derive(Default)]
pub struct ChatListModel {
    pub chats: Vec<ChatSummary>,
    /// Indexes into `chats` surviving filter+query; rebuilt on any change.
    pub visible_cache: Vec<usize>,
    pub loading: bool,
    pub loading_more: bool,
    pub next_after: Option<ChatPageCursor>,
    pub scope: ChatScope,
    pub filter: ChatFilter,
    pub query: String,
    pub selected: Option<String>,
    pub error: Option<String>,
}

impl ChatListModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the current page (first page or invalidation refresh).
    pub fn set_page(&mut self, page: ChatPage) {
        self.chats = page.rows;
        self.next_after = page.next_after;
        self.loading = false;
        self.loading_more = false;
        self.error = None;
        // A refresh replaces only the loaded first page. Preserve an open
        // conversation that may live on a later page; explicit deletion or a
        // scope switch owns clearing selection.
    }

    pub fn set_error(&mut self, message: String) {
        self.loading = false;
        self.loading_more = false;
        self.error = Some(message);
    }

    pub fn append_page(&mut self, page: ChatPage) {
        for chat in page.rows {
            if !self.chats.iter().any(|existing| existing.id == chat.id) {
                self.chats.push(chat);
            }
        }
        self.next_after = page.next_after;
        self.loading_more = false;
        self.error = None;
    }

    /// Keyset cursor reconstructed from the last row of the loaded page.
    /// Kept for chat-list "load more" paging once the list grows beyond one
    /// page; unused by the first shell.
    pub fn next_cursor(&self) -> Option<ChatPageCursor> {
        self.next_after.clone()
    }

    /// Indexes into `chats` of rows surviving the active filter chip and
    /// search query. Product order is pinned first, then recent activity.
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
            let left = &self.chats[*left];
            let right = &self.chats[*right];
            right
                .pinned_at_ms
                .cmp(&left.pinned_at_ms)
                .then_with(|| right.last_activity_ms.cmp(&left.last_activity_ms))
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        visible
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
    use wasabi_domain::{ChatId, ChatPage, ChatSummary};

    use super::{ChatFilter, ChatListModel};

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
    fn visible_uses_deterministic_product_order() {
        let mut model = ChatListModel::new();
        model.set_page(ChatPage {
            rows: vec![
                chat("z@c.us", Some("Zed"), 300, 0, None),
                chat("a@c.us", Some("Alice"), 100, 0, None),
                chat("p@c.us", Some("Pinned"), 1, 0, Some(10)),
                chat("b@c.us", Some("alice"), 200, 0, None),
            ],
            next_after: None,
        });

        assert_eq!(model.visible(), vec![2, 0, 3, 1]);
    }

    #[test]
    fn sorting_happens_after_filter_and_query_without_losing_selection() {
        let mut model = ChatListModel::new();
        model.set_page(ChatPage {
            rows: vec![
                chat("group-a@g.us", Some("Zeta group"), 100, 1, None),
                chat("direct-a@c.us", Some("Alice"), 300, 0, None),
                chat("group-b@g.us", Some("Alpha group"), 200, 2, None),
            ],
            next_after: None,
        });
        model.selected = Some("group-a@g.us".to_string());
        model.filter = ChatFilter::Groups;
        model.query = "group".to_string();

        assert_eq!(model.visible(), vec![2, 0]);

        assert_eq!(model.selected.as_deref(), Some("group-a@g.us"));
        assert_eq!(model.visible(), vec![2, 0]);
    }

    #[test]
    fn append_page_deduplicates_and_advances_cursor() {
        let mut model = ChatListModel::new();
        let first = chat("a@c.us", Some("Alice"), 300, 0, None);
        let second = chat("b@c.us", Some("Bob"), 200, 0, None);
        let next = wasabi_domain::ChatPageCursor {
            pinned_at_ms: None,
            last_activity_ms: second.last_activity_ms,
            chat: second.id.clone(),
        };
        model.set_page(ChatPage {
            rows: vec![first.clone()],
            next_after: Some(wasabi_domain::ChatPageCursor {
                pinned_at_ms: None,
                last_activity_ms: first.last_activity_ms,
                chat: first.id.clone(),
            }),
        });

        model.append_page(ChatPage {
            rows: vec![first, second],
            next_after: Some(next.clone()),
        });

        assert_eq!(model.chats.len(), 2);
        assert_eq!(model.next_cursor(), Some(next));
        assert!(!model.loading_more);
    }

    #[test]
    fn first_page_refresh_preserves_selection_from_a_later_page() {
        let mut model = ChatListModel::new();
        model.selected = Some("later@c.us".to_string());
        model.set_page(ChatPage {
            rows: vec![chat("first@c.us", Some("First"), 300, 0, None)],
            next_after: None,
        });

        assert_eq!(model.selected.as_deref(), Some("later@c.us"));
    }
}

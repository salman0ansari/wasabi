//! Chat list page model: one loaded keyset page plus client-side filtering.

use wasabi_domain::{ChatPageCursor, ChatSummary};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatFilter {
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
    /// Whether the store reported more pages after the loaded one.
    #[allow(dead_code)]
    pub has_more: bool,
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
    /// search query, in store order (newest first).
    pub fn visible(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.chats
            .iter()
            .enumerate()
            .filter(|(_, c)| matches_filter(c, self.filter, &query))
            .map(|(ix, _)| ix)
            .collect()
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

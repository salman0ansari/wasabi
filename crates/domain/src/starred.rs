//! Durable starred-message projections for the product starred viewer.

use crate::message::{MessageRow, PageCursor};

/// One keyset page of starred messages, newest first.
#[derive(Clone, Debug)]
pub struct StarredPage {
    pub hits: Vec<StarredMessageHit>,
    pub next_after: Option<PageCursor>,
    pub has_more: bool,
}

/// One starred row plus the conversation name used to browse it.
#[derive(Clone, Debug)]
pub struct StarredMessageHit {
    pub row: MessageRow,
    pub chat_name: String,
}

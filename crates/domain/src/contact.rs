//! Bounded contact-list projections for New Chat and participant pickers.

use serde::{Deserialize, Serialize};

use crate::{AvatarRef, ChatId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactSummary {
    pub jid: ChatId,
    pub display_name: String,
    pub phone_number: Option<String>,
    pub avatar: Option<AvatarRef>,
}

/// Keyset cursor for deterministic case-folded display-name/JID ordering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactPageCursor {
    pub sort_name: String,
    pub jid: ChatId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactPage {
    pub rows: Vec<ContactSummary>,
    pub next_after: Option<ContactPageCursor>,
}

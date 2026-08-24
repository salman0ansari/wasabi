//! Search projections returned by the product backend.

use serde::{Deserialize, Serialize};

use crate::MessageRow;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchPage {
    pub messages: Vec<MessageSearchHit>,
    pub page: usize,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageSearchHit {
    pub row: MessageRow,
    /// Plain display context around the first match. Styling is owned by the
    /// UI; repository output never contains markup.
    pub snippet: String,
}

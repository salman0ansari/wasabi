//! Device-local chat preferences. These are intentionally separate from
//! protocol-backed pin/archive state and never claim cross-device sync.

use serde::{Deserialize, Serialize};

use crate::MessageId;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub body: String,
    pub reply_to: Option<MessageId>,
    pub staged_attachments: Vec<String>,
    pub edit_target: Option<MessageId>,
}

//! Own-account profile projection. Protocol types stay behind the backend.

use serde::{Deserialize, Serialize};

use crate::{AvatarRef, ChatId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub jid: Option<ChatId>,
    pub push_name: String,
    pub about: Option<String>,
    /// True when About could not be read from cache and the session is offline.
    pub about_needs_refresh: bool,
    pub avatar: Option<AvatarRef>,
}

/// Empty push names are rejected by the protocol; trim and refuse blanks here
/// so the UI can fail before sending.
pub fn parse_push_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        Err("Name cannot be empty")
    } else {
        Ok(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_push_name;

    #[test]
    fn parse_push_name_rejects_empty() {
        assert_eq!(parse_push_name(""), Err("Name cannot be empty"));
        assert_eq!(parse_push_name("   "), Err("Name cannot be empty"));
        assert_eq!(parse_push_name(" Ada "), Ok("Ada".to_string()));
    }
}

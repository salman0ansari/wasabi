//! Protocol privacy categories the desktop can get and set.
//!
//! Wire names match `wacore::iq::privacy`. Unknown server categories never
//! become product values.

use serde::{Deserialize, Serialize};

use crate::ChatId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrivacyCategory {
    Last,
    Online,
    Profile,
    Status,
    GroupAdd,
    ReadReceipts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrivacyValue {
    All,
    Contacts,
    None,
    MatchLastSeen,
    ContactBlacklist,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacySetting {
    pub category: PrivacyCategory,
    pub value: PrivacyValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedContact {
    pub jid: ChatId,
    pub display_name: String,
}

impl PrivacyCategory {
    pub const ALL: [Self; 6] = [
        Self::Last,
        Self::Online,
        Self::Profile,
        Self::Status,
        Self::GroupAdd,
        Self::ReadReceipts,
    ];

    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Last => "last",
            Self::Online => "online",
            Self::Profile => "profile",
            Self::Status => "status",
            Self::GroupAdd => "groupadd",
            Self::ReadReceipts => "readreceipts",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "last" => Some(Self::Last),
            "online" => Some(Self::Online),
            "profile" => Some(Self::Profile),
            "status" => Some(Self::Status),
            "groupadd" => Some(Self::GroupAdd),
            "readreceipts" => Some(Self::ReadReceipts),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Last => "Last seen",
            Self::Online => "Online",
            Self::Profile => "Profile photo",
            Self::Status => "About",
            Self::GroupAdd => "Groups",
            Self::ReadReceipts => "Read receipts",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Last => "Who can see when you were last using WhatsApp.",
            Self::Online => "Who can see when you are online.",
            Self::Profile => "Who can see your profile photo.",
            Self::Status => "Who can see your About text.",
            Self::GroupAdd => "Who can add you to groups.",
            Self::ReadReceipts => {
                "If off, you neither send nor receive read receipts. Does not apply to group chats."
            }
        }
    }

    pub const fn picker_values(self) -> &'static [PrivacyValue] {
        match self {
            Self::Last | Self::Profile | Self::Status | Self::GroupAdd => &[
                PrivacyValue::All,
                PrivacyValue::Contacts,
                PrivacyValue::None,
            ],
            Self::Online => &[PrivacyValue::All, PrivacyValue::MatchLastSeen],
            Self::ReadReceipts => &[PrivacyValue::All, PrivacyValue::None],
        }
    }

    pub fn accepts(self, value: PrivacyValue) -> bool {
        self.picker_values().contains(&value)
            || (value == PrivacyValue::ContactBlacklist
                && matches!(
                    self,
                    Self::Last | Self::Profile | Self::Status | Self::GroupAdd
                ))
    }
}

impl PrivacyValue {
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Contacts => "contacts",
            Self::None => "none",
            Self::MatchLastSeen => "match_last_seen",
            Self::ContactBlacklist => "contact_blacklist",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "contacts" => Some(Self::Contacts),
            "none" => Some(Self::None),
            "match_last_seen" => Some(Self::MatchLastSeen),
            "contact_blacklist" => Some(Self::ContactBlacklist),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "Everyone",
            Self::Contacts => "My contacts",
            Self::None => "Nobody",
            Self::MatchLastSeen => "Same as last seen",
            Self::ContactBlacklist => "My contacts except…",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PrivacyCategory, PrivacyValue};

    #[test]
    fn last_and_read_receipts_roundtrip_on_the_wire() {
        assert_eq!(PrivacyCategory::Last.as_wire(), "last");
        assert_eq!(
            PrivacyCategory::from_wire("last"),
            Some(PrivacyCategory::Last)
        );
        assert_eq!(PrivacyCategory::ReadReceipts.as_wire(), "readreceipts");
        assert_eq!(
            PrivacyCategory::from_wire("readreceipts"),
            Some(PrivacyCategory::ReadReceipts)
        );

        assert_eq!(PrivacyValue::All.as_wire(), "all");
        assert_eq!(PrivacyValue::from_wire("all"), Some(PrivacyValue::All));
        assert_eq!(PrivacyValue::None.as_wire(), "none");
        assert_eq!(PrivacyValue::from_wire("none"), Some(PrivacyValue::None));
        assert_eq!(PrivacyValue::Contacts.as_wire(), "contacts");
        assert_eq!(
            PrivacyValue::from_wire("contacts"),
            Some(PrivacyValue::Contacts)
        );

        assert!(PrivacyCategory::Last.accepts(PrivacyValue::All));
        assert!(PrivacyCategory::Last.accepts(PrivacyValue::Contacts));
        assert!(PrivacyCategory::Last.accepts(PrivacyValue::None));
        assert!(PrivacyCategory::Last.accepts(PrivacyValue::ContactBlacklist));
        assert!(!PrivacyCategory::Last.accepts(PrivacyValue::MatchLastSeen));

        assert!(PrivacyCategory::ReadReceipts.accepts(PrivacyValue::All));
        assert!(PrivacyCategory::ReadReceipts.accepts(PrivacyValue::None));
        assert!(!PrivacyCategory::ReadReceipts.accepts(PrivacyValue::Contacts));
        assert!(!PrivacyCategory::ReadReceipts.accepts(PrivacyValue::MatchLastSeen));
    }
}

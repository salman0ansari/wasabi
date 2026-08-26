//! Bounded contact projections for New Chat and participant pickers.

use std::fmt;

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

/// Validated international phone digits used for a live registration check.
///
/// This value is intentionally separate from [`ContactSummary`]: it may cross
/// the backend boundary before the server confirms that it belongs to an
/// account. Its debug representation is redacted so diagnostics cannot leak a
/// number by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct ContactPhoneNumber(String);

impl ContactPhoneNumber {
    pub fn parse(input: &str) -> Result<Self, &'static str> {
        let digits = input
            .chars()
            .filter(|character| !matches!(character, ' ' | '+' | '-' | '(' | ')'))
            .collect::<String>();
        if !digits.chars().all(|character| character.is_ascii_digit()) {
            return Err("Use digits only, including the country code");
        }
        if !(7..=15).contains(&digits.len()) {
            return Err("Enter 7–15 digits, including the country code");
        }
        if digits.starts_with('0') {
            return Err("Start with the country code, without a leading zero");
        }
        Ok(Self(digits))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ContactPhoneNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContactPhoneNumber([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ContactLookupResult {
    Registered(ContactSummary),
    NotRegistered,
}

impl fmt::Debug for ContactLookupResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registered(_) => formatter.write_str("Registered([REDACTED])"),
            Self::NotRegistered => formatter.write_str("NotRegistered"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_phone_normalizes_common_formatting() {
        let phone = ContactPhoneNumber::parse("+1 (555) 123-4567").unwrap();
        assert_eq!(phone.as_str(), "15551234567");
    }

    #[test]
    fn contact_phone_rejects_invalid_or_local_numbers() {
        assert!(ContactPhoneNumber::parse("+12 abc").is_err());
        assert!(ContactPhoneNumber::parse("123").is_err());
        assert!(ContactPhoneNumber::parse("09123456789").is_err());
        assert!(ContactPhoneNumber::parse("1234567890123456").is_err());
    }

    #[test]
    fn lookup_inputs_and_results_are_redacted_in_debug_output() {
        let phone = ContactPhoneNumber::parse("15551234567").unwrap();
        let result = ContactLookupResult::Registered(ContactSummary {
            jid: ChatId::new("15551234567@s.whatsapp.net"),
            display_name: "+15551234567".to_string(),
            phone_number: Some("15551234567".to_string()),
            avatar: None,
        });
        assert!(!format!("{phone:?}").contains("15551234567"));
        assert!(!format!("{result:?}").contains("15551234567"));
    }
}

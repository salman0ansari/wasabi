//! Ephemeral pairing values shared by the desktop surface and backend.

use std::fmt;
use std::time::Duration;

/// User-facing copy when WhatsApp throttles this companion. Pairing and
/// connect recovery both render this sentence; it must not include protocol
/// `Debug`, IQ text, or the phone number that was entered.
pub const RATE_LIMITED_DEVICE: &str =
    "WhatsApp is rate-limiting this device. Wait, then try again.";

/// Validated E.164-like phone digits used to request a companion link code.
///
/// The value deliberately redacts its `Debug` representation so future
/// diagnostics cannot accidentally leak a phone number.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingPhoneNumber(String);

impl PairingPhoneNumber {
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

impl fmt::Debug for PairingPhoneNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingPhoneNumber([REDACTED])")
    }
}

/// A server-issued companion link code and its remaining validity.
///
/// This projection is never serialized or persisted. Its `Debug` output is
/// redacted for the same reason as [`PairingPhoneNumber`].
#[derive(Clone, PartialEq, Eq)]
pub struct PhonePairCode {
    pub code: String,
    pub expires_in: Duration,
}

impl fmt::Debug for PhonePairCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PhonePairCode")
            .field("code", &"[REDACTED]")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_phone_formatting() {
        let phone = PairingPhoneNumber::parse("+1 (555) 123-4567").unwrap();
        assert_eq!(phone.as_str(), "15551234567");
    }

    #[test]
    fn rejects_invalid_or_implausible_phone_numbers() {
        assert!(PairingPhoneNumber::parse("+12 abc").is_err());
        assert!(PairingPhoneNumber::parse("123").is_err());
        assert!(PairingPhoneNumber::parse("09123456789").is_err());
        assert!(PairingPhoneNumber::parse("1234567890123456").is_err());
    }

    #[test]
    fn sensitive_values_are_redacted_in_debug_output() {
        let phone = PairingPhoneNumber::parse("15551234567").unwrap();
        let code = PhonePairCode {
            code: "ABCD1234".to_string(),
            expires_in: Duration::from_secs(180),
        };
        assert!(!format!("{phone:?}").contains("15551234567"));
        assert!(!format!("{code:?}").contains("ABCD1234"));
    }
}

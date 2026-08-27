//! Immutable product commands for group creation.

use std::collections::HashSet;
use std::fmt;

use crate::ChatId;

pub const GROUP_SUBJECT_MAX_CHARS: usize = 100;
pub const GROUP_INVITEE_MAX: usize = 256;

/// A fully validated group-creation request captured before async dispatch.
///
/// Subject and participant identities are deliberately redacted from Debug so
/// command diagnostics remain content-free and JID-free.
#[derive(Clone, PartialEq, Eq)]
pub struct CreateGroupRequest {
    subject: String,
    participants: Vec<ChatId>,
}

impl CreateGroupRequest {
    pub fn new(
        subject: impl Into<String>,
        participants: Vec<ChatId>,
    ) -> Result<Self, &'static str> {
        let subject = subject.into().trim().to_string();
        if subject.is_empty() {
            return Err("Enter a group name");
        }
        if subject.chars().count() > GROUP_SUBJECT_MAX_CHARS {
            return Err("Group names can contain up to 100 characters");
        }

        let mut seen = HashSet::with_capacity(participants.len());
        let participants = participants
            .into_iter()
            .filter(|participant| seen.insert(participant.clone()))
            .collect::<Vec<_>>();
        if participants.is_empty() {
            return Err("Select at least one participant");
        }
        if participants.len() > GROUP_INVITEE_MAX {
            return Err("A group can include up to 256 invited participants");
        }
        Ok(Self {
            subject,
            participants,
        })
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn participants(&self) -> &[ChatId] {
        &self.participants
    }
}

impl fmt::Debug for CreateGroupRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateGroupRequest")
            .field("subject", &"[REDACTED]")
            .field("participant_count", &self.participants.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_trims_subject_and_deduplicates_participants() {
        let request = CreateGroupRequest::new(
            "  Weekend plans  ",
            vec![
                ChatId::new("1@s.whatsapp.net"),
                ChatId::new("1@s.whatsapp.net"),
                ChatId::new("2@lid"),
            ],
        )
        .unwrap();
        assert_eq!(request.subject(), "Weekend plans");
        assert_eq!(request.participants().len(), 2);
    }

    #[test]
    fn request_rejects_empty_long_or_participant_free_groups() {
        assert!(CreateGroupRequest::new(" ", vec![ChatId::new("1@s.whatsapp.net")]).is_err());
        assert!(
            CreateGroupRequest::new(
                "x".repeat(GROUP_SUBJECT_MAX_CHARS + 1),
                vec![ChatId::new("1@s.whatsapp.net")]
            )
            .is_err()
        );
        assert!(CreateGroupRequest::new("Friends", Vec::new()).is_err());
    }

    #[test]
    fn debug_output_contains_neither_subject_nor_jids() {
        let request = CreateGroupRequest::new(
            "Secret launch",
            vec![ChatId::new("15551234567@s.whatsapp.net")],
        )
        .unwrap();
        let debug = format!("{request:?}");
        assert!(!debug.contains("Secret launch"));
        assert!(!debug.contains("15551234567"));
        assert!(debug.contains("participant_count"));
    }
}

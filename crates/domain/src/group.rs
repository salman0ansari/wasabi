//! Immutable product commands for group creation and management.

use std::collections::HashSet;
use std::fmt;

use crate::{ChatId, GroupDetails};

pub const GROUP_SUBJECT_MAX_CHARS: usize = 100;
pub const GROUP_DESCRIPTION_MAX_CHARS: usize = 2048;
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

/// Fetch or revoke a group's invite link. GET is not a metadata mutation.
///
/// The group identity is captured before async dispatch. Debug redacts the
/// chat so diagnostics cannot leak a JID or the resulting URL.
#[derive(Clone, PartialEq, Eq)]
pub struct GroupInviteLinkRequest {
    chat: ChatId,
    reset: bool,
}

impl GroupInviteLinkRequest {
    pub fn new(chat: ChatId, reset: bool) -> Self {
        Self { chat, reset }
    }

    pub fn chat(&self) -> &ChatId {
        &self.chat
    }

    pub fn reset(&self) -> bool {
        self.reset
    }
}

impl fmt::Debug for GroupInviteLinkRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GroupInviteLinkRequest")
            .field("chat", &"[REDACTED]")
            .field("reset", &self.reset)
            .finish()
    }
}

/// One validated, immutable mutation against an exact group identity.
///
/// Text and participant identities are redacted from `Debug`; callers may log
/// the operation kind and cardinality without leaking account content.
#[derive(Clone, PartialEq, Eq)]
pub struct GroupPatch {
    chat: ChatId,
    change: GroupChange,
}

#[derive(Clone, PartialEq, Eq)]
pub enum GroupChange {
    Subject(String),
    Description(Option<String>),
    OnlyAdminsEdit(bool),
    OnlyAdminsSend(bool),
    MembershipApproval(bool),
    AddParticipants(Vec<ChatId>),
    RemoveParticipant(ChatId),
    PromoteParticipant(ChatId),
    DemoteParticipant(ChatId),
    ApproveMembershipRequest(ChatId),
    RejectMembershipRequest(ChatId),
    Leave,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPatchResult {
    /// Fresh server metadata after an acknowledged mutation. Leaving a group
    /// has no remaining details and therefore returns `None`.
    pub details: Option<GroupDetails>,
    pub applied_participants: usize,
    pub rejected_participants: usize,
}

impl GroupPatch {
    pub fn subject(chat: ChatId, subject: impl Into<String>) -> Result<Self, &'static str> {
        let subject = subject.into().trim().to_string();
        if subject.is_empty() {
            return Err("Enter a group name");
        }
        if subject.chars().count() > GROUP_SUBJECT_MAX_CHARS {
            return Err("Group names can contain up to 100 characters");
        }
        Ok(Self {
            chat,
            change: GroupChange::Subject(subject),
        })
    }

    /// An empty or whitespace-only value removes the description.
    pub fn description(chat: ChatId, description: impl Into<String>) -> Result<Self, &'static str> {
        let description = description.into().trim().to_string();
        if description.chars().count() > GROUP_DESCRIPTION_MAX_CHARS {
            return Err("Group descriptions can contain up to 2048 characters");
        }
        Ok(Self {
            chat,
            change: GroupChange::Description((!description.is_empty()).then_some(description)),
        })
    }

    pub fn only_admins_edit(chat: ChatId, enabled: bool) -> Self {
        Self {
            chat,
            change: GroupChange::OnlyAdminsEdit(enabled),
        }
    }

    pub fn only_admins_send(chat: ChatId, enabled: bool) -> Self {
        Self {
            chat,
            change: GroupChange::OnlyAdminsSend(enabled),
        }
    }

    pub fn membership_approval(chat: ChatId, enabled: bool) -> Self {
        Self {
            chat,
            change: GroupChange::MembershipApproval(enabled),
        }
    }

    pub fn add_participants(chat: ChatId, participants: Vec<ChatId>) -> Result<Self, &'static str> {
        let mut seen = HashSet::with_capacity(participants.len());
        let participants = participants
            .into_iter()
            .filter(|participant| seen.insert(participant.clone()))
            .collect::<Vec<_>>();
        if participants.is_empty() {
            return Err("Select at least one participant");
        }
        if participants.len() > GROUP_INVITEE_MAX {
            return Err("Add up to 256 participants at a time");
        }
        Ok(Self {
            chat,
            change: GroupChange::AddParticipants(participants),
        })
    }

    pub fn remove_participant(chat: ChatId, participant: ChatId) -> Self {
        Self {
            chat,
            change: GroupChange::RemoveParticipant(participant),
        }
    }

    pub fn promote_participant(chat: ChatId, participant: ChatId) -> Self {
        Self {
            chat,
            change: GroupChange::PromoteParticipant(participant),
        }
    }

    pub fn demote_participant(chat: ChatId, participant: ChatId) -> Self {
        Self {
            chat,
            change: GroupChange::DemoteParticipant(participant),
        }
    }

    pub fn approve_membership_request(chat: ChatId, participant: ChatId) -> Self {
        Self {
            chat,
            change: GroupChange::ApproveMembershipRequest(participant),
        }
    }

    pub fn reject_membership_request(chat: ChatId, participant: ChatId) -> Self {
        Self {
            chat,
            change: GroupChange::RejectMembershipRequest(participant),
        }
    }

    pub fn leave(chat: ChatId) -> Self {
        Self {
            chat,
            change: GroupChange::Leave,
        }
    }

    pub fn chat(&self) -> &ChatId {
        &self.chat
    }

    pub fn change(&self) -> &GroupChange {
        &self.change
    }
}

impl fmt::Debug for GroupPatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, participant_count) = match &self.change {
            GroupChange::Subject(_) => ("subject", None),
            GroupChange::Description(_) => ("description", None),
            GroupChange::OnlyAdminsEdit(_) => ("only_admins_edit", None),
            GroupChange::OnlyAdminsSend(_) => ("only_admins_send", None),
            GroupChange::MembershipApproval(_) => ("membership_approval", None),
            GroupChange::AddParticipants(participants) => {
                ("add_participants", Some(participants.len()))
            }
            GroupChange::RemoveParticipant(_) => ("remove_participant", Some(1)),
            GroupChange::PromoteParticipant(_) => ("promote_participant", Some(1)),
            GroupChange::DemoteParticipant(_) => ("demote_participant", Some(1)),
            GroupChange::ApproveMembershipRequest(_) => ("approve_membership_request", Some(1)),
            GroupChange::RejectMembershipRequest(_) => ("reject_membership_request", Some(1)),
            GroupChange::Leave => ("leave", None),
        };
        formatter
            .debug_struct("GroupPatch")
            .field("chat", &"[REDACTED]")
            .field("kind", &kind)
            .field("participant_count", &participant_count)
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

    #[test]
    fn invite_link_request_debug_contains_neither_url_nor_jid() {
        let request = GroupInviteLinkRequest::new(ChatId::new("120363000000000001@g.us"), true);
        assert!(request.reset());
        assert_eq!(request.chat().as_str(), "120363000000000001@g.us");
        let debug = format!("{request:?}");
        assert!(!debug.contains("120363"));
        assert!(!debug.contains("@g.us"));
        assert!(!debug.contains("chat.whatsapp.com"));
        assert!(!debug.contains("https://"));
        assert!(debug.contains("reset: true"));
    }

    #[test]
    fn group_patches_validate_and_redact_content() {
        let chat = ChatId::new("120363000000000001@g.us");
        let patch = GroupPatch::subject(chat.clone(), " Secret launch ").unwrap();
        assert!(matches!(patch.change(), GroupChange::Subject(value) if value == "Secret launch"));
        let debug = format!("{patch:?}");
        assert!(!debug.contains("Secret launch"));
        assert!(!debug.contains("120363"));

        assert!(GroupPatch::subject(chat.clone(), " ").is_err());
        assert!(
            GroupPatch::description(chat.clone(), "x".repeat(GROUP_DESCRIPTION_MAX_CHARS + 1))
                .is_err()
        );
        assert!(matches!(
            GroupPatch::description(chat, "  ").unwrap().change(),
            GroupChange::Description(None)
        ));
    }

    #[test]
    fn participant_patches_deduplicate_and_hide_identities() {
        let chat = ChatId::new("120363000000000001@g.us");
        let first = ChatId::new("15550000001@s.whatsapp.net");
        let second = ChatId::new("15550000002@s.whatsapp.net");
        let patch =
            GroupPatch::add_participants(chat, vec![first.clone(), second.clone(), first]).unwrap();

        assert!(matches!(
            patch.change(),
            GroupChange::AddParticipants(participants) if participants.len() == 2
        ));
        let debug = format!("{patch:?}");
        assert!(debug.contains("participant_count"));
        assert!(!debug.contains("15550000001"));
        assert!(!debug.contains("120363"));
        assert!(GroupPatch::add_participants(ChatId::new("group@g.us"), Vec::new()).is_err());
    }

    #[test]
    fn membership_request_patches_hide_identities() {
        let chat = ChatId::new("120363000000000001@g.us");
        let person = ChatId::new("15550000001@s.whatsapp.net");
        let approve = GroupPatch::approve_membership_request(chat.clone(), person.clone());
        let reject = GroupPatch::reject_membership_request(chat, person);
        assert!(matches!(
            approve.change(),
            GroupChange::ApproveMembershipRequest(_)
        ));
        assert!(matches!(
            reject.change(),
            GroupChange::RejectMembershipRequest(_)
        ));
        for patch in [approve, reject] {
            let debug = format!("{patch:?}");
            assert!(debug.contains("participant_count"));
            assert!(!debug.contains("15550000001"));
            assert!(!debug.contains("120363"));
        }
    }
}

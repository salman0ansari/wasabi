//! Contact and group projections used by the on-demand information drawer.
//! Protocol and database types are deliberately absent from this boundary.

use serde::{Deserialize, Serialize};

use crate::ChatId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatKind {
    Direct,
    Group,
    Newsletter,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvatarRef(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectContactDetails {
    pub jid: String,
    pub display_name: String,
    pub phone_number: Option<String>,
    pub about: Option<String>,
    pub avatar: Option<AvatarRef>,
    pub is_blocked: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipantRole {
    Member,
    Admin,
    SuperAdmin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub jid: String,
    pub display_name: String,
    pub avatar: Option<AvatarRef>,
    pub role: ParticipantRole,
    pub is_self: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPermissions {
    pub only_admins_edit: bool,
    pub only_admins_send: bool,
    pub membership_approval: bool,
    pub current_user_role: Option<ParticipantRole>,
}

impl GroupPermissions {
    pub fn can_manage_members(&self) -> bool {
        matches!(
            self.current_user_role,
            Some(ParticipantRole::Admin | ParticipantRole::SuperAdmin)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingMembershipRequest {
    pub jid: ChatId,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupDetails {
    pub chat: ChatId,
    pub subject: String,
    pub description: Option<String>,
    pub avatar: Option<AvatarRef>,
    pub participant_count: usize,
    pub participants: Vec<Participant>,
    pub permissions: GroupPermissions,
}

/// A cached group whose participant snapshot includes a given direct contact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedGroup {
    pub chat: ChatId,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationDetails {
    Direct(DirectContactDetails),
    Group(GroupDetails),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permissions(role: Option<ParticipantRole>) -> GroupPermissions {
        GroupPermissions {
            only_admins_edit: false,
            only_admins_send: false,
            membership_approval: false,
            current_user_role: role,
        }
    }

    #[test]
    fn can_manage_members_is_admin_or_super_admin() {
        assert!(permissions(Some(ParticipantRole::Admin)).can_manage_members());
        assert!(permissions(Some(ParticipantRole::SuperAdmin)).can_manage_members());
        assert!(!permissions(Some(ParticipantRole::Member)).can_manage_members());
        assert!(!permissions(None).can_manage_members());
    }
}

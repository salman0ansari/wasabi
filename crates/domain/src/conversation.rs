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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationDetails {
    Direct(DirectContactDetails),
    Group(GroupDetails),
}

//! Wasabi domain types.
//!
//! Pure, headless, GPUI-free. Everything the UI and the core exchange that is
//! durable-shaped lives here; protocol types never cross this boundary
//!.

pub mod account;
pub mod actions;
pub mod contact;
pub mod conversation;
pub mod error;
pub mod group;
pub mod ids;
pub mod media;
pub mod message;
pub mod notification;
pub mod page;
pub mod pairing;
pub mod preferences;
pub mod presence;
pub mod privacy;
pub mod search;
pub mod send;
pub mod starred;

pub use account::{AccountProfile, parse_push_name};
pub use actions::{ChatAction, ContactAction, MessageAction, MessageActionTarget};
pub use contact::{
    ContactLookupResult, ContactPage, ContactPageCursor, ContactPhoneNumber, ContactSummary,
};
pub use conversation::{
    AvatarRef, ChatKind, ConversationDetails, DirectContactDetails, GroupDetails, GroupPermissions,
    Participant, ParticipantRole, PendingMembershipRequest, SharedGroup,
};
pub use error::{ErrorKind, ServiceError};
pub use group::{
    CreateGroupRequest, GROUP_DESCRIPTION_MAX_CHARS, GROUP_INVITEE_MAX, GROUP_SUBJECT_MAX_CHARS,
    GroupChange, GroupInviteLinkRequest, GroupPatch, GroupPatchResult,
};
pub use ids::{AccountId, ChatId, LocalCursor, MediaId, MessageId, TransferId};
pub use media::{
    AttachmentKind, CachedAvatar, CachedMedia, MediaDownloadRequest, ProfilePictureRequest,
    StagedAttachment, TransferDirection, TransferJob, TransferPayload, TransferState,
};
pub use message::{
    ChatSummary, MESSAGE_EDIT_WINDOW_MS, MediaAvailability, MediaDescriptor, MessageContext,
    MessageDirection, MessageKind, MessagePage, MessageRow, MessageStatus, PageCursor,
    QuotedMessage, ReactionActor, ReactionSummary, ReceiptActor, SenderJid,
    UnavailableMessageReason,
};
pub use notification::NotificationCandidate;
pub use page::{ChatPage, ChatPageCursor, ChatScope};
pub use pairing::{PairingPhoneNumber, PhonePairCode, RATE_LIMITED_DEVICE};
pub use preferences::Draft;
pub use presence::{TypingState, TypingUpdate};
pub use privacy::{BlockedContact, PrivacyCategory, PrivacySetting, PrivacyValue};
pub use search::{MessageSearchHit, SearchPage};
pub use send::{SendContent, SendReceipt, SendRequest};
pub use starred::{StarredMessageHit, StarredPage};

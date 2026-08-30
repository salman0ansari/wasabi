//! UI models over durable state. Every mutation happens in entity update
//! closures guarded by generation counters owned by the views.

pub(crate) mod chats;
pub(crate) mod messages;
#[cfg(debug_assertions)]
pub(crate) mod preview;
pub(crate) mod session;
pub(crate) mod settings;

pub use chats::ChatListModel;
pub use messages::MessageWindowModel;
pub use session::{RecoveryAction, RecoveryCopy, SessionMirror};
pub use settings::{DeviceSettings, SettingsSection, ThemePreference};

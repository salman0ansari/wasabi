//! UI models over durable state. Every mutation happens in entity update
//! closures guarded by generation counters owned by the views.

pub(crate) mod chats;
pub(crate) mod messages;
pub(crate) mod session;
pub(crate) mod settings;

pub use chats::ChatListModel;
pub use messages::MessageWindowModel;
pub use session::SessionMirror;
pub use settings::{DeviceSettings, SettingsSection, ThemePreference};

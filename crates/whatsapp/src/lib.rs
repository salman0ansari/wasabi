//! Wasabi WhatsApp adapter: assembles the vendored `whatsapp-rust` stack and
//! maps it onto wasabi's service contracts. Protocol types stop here.

pub mod durability;
pub mod history;
pub mod lifecycle;
pub mod normalize;
pub mod outbox;
pub mod session;

pub use durability::RepositoryDurabilityHook;
pub use session::{AccountSession, SessionConfig};

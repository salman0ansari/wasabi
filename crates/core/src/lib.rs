//! Wasabi core: execution-domain ownership.
//!
//! `CoreSupervisor` exclusively owns the process Tokio runtime, the
//! cancellation hierarchy, and the deterministic shutdown sequence
//!. GPUI never touches tokio primitives directly.

pub mod events;
pub mod runtime;
pub mod state;
pub mod supervisor;

pub use events::{ConnectionStateWatch, InvalidationFeed};
pub use runtime::{CoreRuntime, RuntimeConfig};
pub use state::SessionState;
pub use supervisor::{CoreSupervisor, SupervisorConfig, SupervisorError};

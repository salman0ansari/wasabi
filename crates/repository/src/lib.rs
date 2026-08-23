//! Wasabi repository: per-account SQLite ownership and the query facade the
//! rest of the product depends on (charter §31). ChatStore specifics never
//! leak past this crate.

pub mod config;
pub mod store;

pub use config::{StorageLayout, StoreTuning};
pub use store::{AccountStore, OpenError};

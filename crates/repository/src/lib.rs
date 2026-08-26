//! Wasabi repository: per-account SQLite ownership and the query facade the
//! rest of the product depends on. ChatStore specifics never
//! leak past this crate.

pub mod config;
mod contacts;
pub mod preferences;
pub mod search;
pub mod store;
pub mod transfers;
mod wasabi_schema;

pub use config::{StorageLayout, StoreTuning};
pub use store::{AccountStore, OpenError, StoreChange, StoreChangeFeed};

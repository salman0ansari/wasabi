//! Desktop shell views. All rendering reads prepared model fields; no IO or
//! queries happen inside render paths.

mod chat_list;
mod composer;
mod conversation;
mod new_chat;
mod pairing;
mod right_panel;
mod root;
mod settings;

pub use root::{BridgeGlobal, MainWindow, key_bindings};

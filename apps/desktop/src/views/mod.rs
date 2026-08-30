//! Desktop shell views. All rendering reads prepared model fields; no IO or
//! queries happen inside render paths.

mod avatar;
mod chat_list;
mod composer;
mod conversation;
mod new_chat;
mod new_group;
mod pairing;
mod right_panel;
mod root;
mod settings;

pub use root::{BridgeGlobal, MainWindow, key_bindings};

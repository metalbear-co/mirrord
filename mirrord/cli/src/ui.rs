//! `mirrord ui` command entrypoint. The full session-monitor implementation lives in
//! [`ui_impl.rs`]; this file is the small module shim that imports it under a fixed name.

#[path = "ui_impl.rs"]
mod imp;

pub use imp::ui_command;

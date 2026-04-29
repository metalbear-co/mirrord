//! `mirrord ui` command entrypoint.
//!
//! The full session-monitor implementation lives in `ui_impl.rs` and is unix-only because it
//! depends on `mirrord-session-monitor-client`, which talks to per-session HTTP APIs over Unix
//! domain sockets. On Windows the command exists but returns an error until the transport has
//! been ported (see `mirrord/intproxy/tests/session_monitor_round_trip.rs` for the contract any
//! future port must satisfy).

#[cfg(unix)]
#[path = "ui_impl.rs"]
mod imp;

#[cfg(unix)]
pub use imp::ui_command;

#[cfg(windows)]
pub async fn ui_command(_args: crate::config::UiArgs) -> Result<(), crate::error::CliError> {
    Err(crate::error::CliError::UiError(
        "mirrord ui is not yet supported on Windows".to_owned(),
    ))
}

//! The `mirrord tui` command - the terminal interface, implemented in the `mirrord-tui` crate.

use miette::Diagnostic;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_tui::TelemetrySession;
use thiserror::Error;

use crate::{CliResult, user_data::UserData};

#[derive(Debug, Error, Diagnostic)]
pub enum TuiCliError {
    /// The interface reports failures inside itself (no cluster connection, a watch that died) on
    /// its own screen and keeps running, so anything that reaches here ended the session.
    #[error("The mirrord TUI exited with an error: {0}")]
    // Not `MIRRORD_LOG_FILE`: that belongs to the standalone `mirrord-tui` binary, which owns its
    // own logger. Here the CLI's logger is already installed and writes to stderr - which the
    // interface takes over for the duration of the session - so the console is the way to read the
    // logs back.
    #[diagnostic(help(
        "Run `mirrord-console` and set `MIRRORD_CONSOLE_ADDR` to capture this session's logs: the \
         interface owns the terminal's stderr while it runs, so the CLI's usual stderr logging \
         cannot be read back from it."
    ))]
    // Boxed rather than kept as the `anyhow::Error` the interface returns, so that the CLI - which
    // reports errors through `thiserror`/`miette` throughout - does not take on `anyhow` for one
    // variant. The conversion keeps the whole `source` chain.
    Exited(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

/// The `mirrord tui` command handler.
pub(crate) async fn tui_command(watch: drain::Watch, user_data: &UserData) -> CliResult<()> {
    // Read here for the `telemetry` opt-out; the interface resolves the config again per
    // connection, for the settings that a change of scope can change while it runs. Resolving up
    // front is also what reports a broken config as the config error it is, the way every other
    // subcommand does - left to the interface, the same error would surface as a failure to
    // connect, once the alternate screen is already up and pointing at the wrong problem.
    let config = LayerConfig::resolve(&mut ConfigContext::default())?;

    let telemetry = TelemetrySession {
        enabled: config.telemetry,
        machine_id: user_data.machine_id(),
        watch,
    };

    mirrord_tui::run(Some(telemetry))
        .await
        .map_err(|error| TuiCliError::Exited(error.into()))?;

    Ok(())
}

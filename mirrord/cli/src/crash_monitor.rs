//! The internal `mirrord crash-monitor` subcommand.
//!
//! One monitor runs per session on Windows. It accepts layer registrations over TCP, writes an
//! out-of-process minidump when a registered process signals a crash, and logs the exit code when a
//! registered process dies with no handler firing (the external-kill case).
//!
//! The heavy lifting lives in [`utils_win::diagnostics::monitor`]. This module only binds the
//! socket, reports its address to the parent CLI, and runs the server.
//!
//! NOTE: more of the Windows-specific plumbing that still lives in the CLI (for instance next to
//! the UI/token code) should migrate into `utils-win`, so platform logic has a single home instead
//! of being spread across the CLI.

use std::{
    io::{self, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener},
};

use mirrord_config::MIRRORD_CRASH_EPHEMERAL_DIR;
use tracing_subscriber::EnvFilter;
use utils_win::diagnostics::{
    crash_dir, full_memory_dump,
    monitor::{MonitorConfig, serve},
};

use crate::error::{CliError, CliResult};

/// Runs the crash-dump monitor until the session ends.
///
/// It binds a localhost registration socket, prints the bound address to stdout for the parent CLI
/// to forward to the layers, then serves on a blocking task.
///
/// # Arguments
///
/// * `port` - the registration port. `0` binds an ephemeral port.
/// * `root_pid` - the root CLI process id, recorded for the process-tree report.
pub(crate) async fn monitor(port: u16, root_pid: u32) -> CliResult<()> {
    init_tracing();

    let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))
        .map_err(|error| CliError::CrashMonitorError(format!("failed to bind: {error}")))?;
    let address = listener.local_addr().map_err(|error| {
        CliError::CrashMonitorError(format!("failed to read local address: {error}"))
    })?;

    // The parent CLI reads this line off our stdout and forwards it to the layers.
    let mut stdout = io::stdout();
    writeln!(stdout, "{address}")
        .and_then(|()| stdout.flush())
        .map_err(|error| {
            CliError::CrashMonitorError(format!("failed to write address: {error}"))
        })?;

    let config = MonitorConfig {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        dump_directory: crash_dir(),
        full_memory: full_memory_dump(),
        // The CLI sets this when it made an ephemeral session dir; the monitor removes it on a
        // clean exit.
        ephemeral: std::env::var_os(MIRRORD_CRASH_EPHEMERAL_DIR).is_some(),
    };

    tracing::info!(%address, root_pid, "crash monitor started");

    tokio::task::spawn_blocking(move || serve(listener, root_pid, config))
        .await
        .map_err(|error| CliError::CrashMonitorError(format!("monitor task failed: {error}")))?
        .map_err(|error| CliError::CrashMonitorError(format!("monitor server error: {error}")))?;

    Ok(())
}

/// Sets up a stderr tracing subscriber for the monitor process.
///
/// The monitor's stderr is inherited by the parent, so these lines appear in the user's console.
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();
}

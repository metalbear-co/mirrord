use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use nix::unistd::{ForkResult, fork, setsid};

use crate::error::{RemoteBootstrapError, Result};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn spawn(binary: &Path) -> Result<()> {
    tracing::info!(agent_binary = %binary.display(), "Launching workload-companion agent");

    let mut command = Command::new(binary);
    command
        .arg("workload-companion")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Ok(log_level) = std::env::var("MIRRORD_AGENT_RUST_LOG") {
        command.env("RUST_LOG", log_level);
    }

    spawn_daemon(command)
}

fn spawn_daemon(mut command: Command) -> Result<()> {
    // The workload-companion belongs to the container rather than the process that happened to
    // receive `LD_PRELOAD`. Detaching it prevents wrappers such as `tini` from coupling its
    // lifetime to an application process; the container runtime terminates it with the container.
    // `Command::spawn` performs the first fork, and the `pre_exec` hook performs the second.
    unsafe {
        command.pre_exec(|| {
            setsid()?;
            match fork()? {
                ForkResult::Parent { .. } => std::process::exit(0),
                ForkResult::Child => Ok(()),
            }
        });
    }

    let mut intermediate_child = command.spawn()?;
    let status = intermediate_child.wait()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "workload-companion daemonization process exited with {status}"
        ))
        .into());
    }

    tracing::info!("Spawned daemonized workload-companion agent");
    Ok(())
}

pub(crate) fn wait_until_ready(handoff_socket: &Path) -> Result<()> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;

    while Instant::now() < deadline {
        if handoff_socket.exists() {
            tracing::debug!(file = %handoff_socket.display(), "Workload-companion is ready");
            return Ok(());
        }

        sleep(STARTUP_POLL_INTERVAL);
    }

    Err(RemoteBootstrapError::AgentTimeout(
        handoff_socket.to_owned(),
        STARTUP_TIMEOUT.as_secs(),
    ))
}

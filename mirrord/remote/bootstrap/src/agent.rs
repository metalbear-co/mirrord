use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

use nix::unistd::{ForkResult, fork, setsid};

use crate::error::Result;

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

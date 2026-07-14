#![cfg(target_os = "linux")]
use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

use ctor::ctor;
use mirrord_layer_lib::logging::init_tracing;

use crate::error::Result;

mod error;
mod extract;

#[ctor]
fn mirrord_layer_bootstrap_entry_point() {
    init_tracing();
    tracing::info!("Starting serverless bootstrap");

    // make sure no other process loads bootstrap
    unsafe { std::env::remove_var("LD_PRELOAD") };
    tracing::trace!("Removed LD_PRELOAD before spawning agent");

    if let Err(error) = spawn_remote_flow() {
        tracing::error!(%error, "Failed to initialize serverless bootstrap");
    }
}

fn spawn_remote_flow() -> Result<()> {
    let agent_binary = extract::extract_agent_binary()?;

    tracing::info!(agent_binary = %agent_binary.display(), "Launching sidecar agent");

    spawn_agent(&agent_binary)?;

    Ok(())
}

fn spawn_agent(binary: &Path) -> Result<()> {
    let mut command = Command::new(binary);
    command
        .arg("sidecar")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Ok(log_level) = std::env::var("MIRRORD_AGENT_RUST_LOG") {
        command.env("RUST_LOG", log_level);
    }

    // makes the spawned agent receive `SIGTERM` if the bootstrap process exits
    unsafe {
        command.pre_exec(|| {
            let result = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            if result == -1 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(())
        });
    }

    let child = command.spawn()?;
    tracing::info!(pid = child.id(), "Spawned sidecar agent");
    Ok(())
}

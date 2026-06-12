#![cfg_attr(not(target_os = "linux"), allow(unused))]

#[cfg(not(target_os = "linux"))]
compile_error!("mirrord-serverless-bootstrap is supported only on Linux");

use std::process::{Command, Stdio};

use ctor::ctor;
use mirrord_layer_lib::logging::init_tracing;

mod extract;

#[ctor]
fn mirrord_layer_bootstrap_entry_point() {
    init_tracing();
    tracing::info!("Starting serverless bootstrap");

    // make sure no other process loads bootstrap
    unsafe { std::env::remove_var("LD_PRELOAD") };
    tracing::trace!("Removed LD_PRELOAD before spawning agent");

    if let Err(error) = spawn_agent() {
        tracing::error!(%error, "Failed to spawn sidecar agent");
    }
}

fn spawn_agent() -> Result<(), String> {
    let agent_binary = extract::extract_agent_binary()?;
    tracing::info!(agent_binary = %agent_binary.display(), "Launching sidecar agent");

    let mut command = Command::new(&agent_binary);
    command
        .arg("sidecar")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let child = command
        .spawn()
        .map_err(|error| format!("failed spawning sidecar process: {error}"))?;

    tracing::info!(pid = child.id(), "Spawned sidecar agent");
    Ok(())
}

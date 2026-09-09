#![cfg(target_os = "linux")]

use std::{io, os::unix::net::UnixStream, path::PathBuf};

use ctor::ctor;
use mirrord_layer_lib::logging::init_tracing;
use mirrord_remote_layer_protocol::CONNECTION_HANDOFF_SOCKET_ENV;
use tempfile::Builder;

use crate::error::Result;

mod agent;
mod error;
mod extract;
mod remote_layer;

const CONNECTION_HANDOFF_SOCKET: &str = "connection-handoff.sock";

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
    let connection_handoff_socket = prepare_handoff_socket()?;

    let agent_binary = extract::extract_agent_binary()?;
    agent::spawn(&agent_binary)?;
    agent::wait_until_ready(&connection_handoff_socket)?;

    let remote_layer_binary = extract::extract_remote_layer_binary()?;
    remote_layer::load(&remote_layer_binary)?;

    Ok(())
}

/// Allocates a handoff socket path and publishes it for the spawned processes.
fn prepare_handoff_socket() -> Result<PathBuf> {
    let path = match std::env::var(CONNECTION_HANDOFF_SOCKET_ENV) {
        Ok(path) => prepare_explicit_handoff_socket(path.into())?,
        Err(_) => prepare_generated_handoff_socket()?,
    };

    unsafe { std::env::set_var(CONNECTION_HANDOFF_SOCKET_ENV, &path) };
    Ok(path)
}

fn prepare_explicit_handoff_socket(path: PathBuf) -> Result<PathBuf> {
    if !path.exists() {
        return Ok(path);
    }

    match UnixStream::connect(&path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "connection handoff socket is already active: {}",
                path.display()
            ),
        )
        .into()),
        Err(_) => {
            std::fs::remove_file(&path)?;
            Ok(path)
        }
    }
}

fn prepare_generated_handoff_socket() -> Result<PathBuf> {
    let run_directory = Builder::new().prefix("mirrord-").tempdir()?.keep();
    Ok(run_directory.join(CONNECTION_HANDOFF_SOCKET))
}

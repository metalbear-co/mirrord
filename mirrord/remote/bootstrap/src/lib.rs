#![cfg(target_os = "linux")]

use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::Path,
};

use ctor::ctor;
use mirrord_layer_lib::logging::init_tracing;
use mirrord_remote_layer_protocol::connection_handoff::prepare_handoff_socket;

use crate::error::Result;

mod agent;
mod error;
mod extract;
mod remote_layer;

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
    configure_preload(&remote_layer_binary);

    Ok(())
}

/// Adds the loaded remote layer to the inherited preload list without reintroducing the bootstrap.
///
/// This runs from the bootstrap constructor, before application threads are started. Keeping the
/// mutation here lets ordinary child-process environment inheritance propagate the remote layer
/// without interposing on every execution function.
fn configure_preload(remote_layer_binary: &Path) {
    let remote_layer = remote_layer_binary.as_os_str().as_bytes();
    let mut preload = std::env::var_os("LD_PRELOAD")
        .map(|value| value.as_os_str().as_bytes().to_owned())
        .unwrap_or_default();

    if !preload
        .split(|byte| *byte == b':')
        .any(|entry| entry == remote_layer)
    {
        if !preload.is_empty() {
            preload.push(b':');
        }
        preload.extend_from_slice(remote_layer);
    }

    // SAFETY: The bootstrap constructor performs this before application threads are started.
    unsafe { std::env::set_var("LD_PRELOAD", OsString::from_vec(preload)) };
}

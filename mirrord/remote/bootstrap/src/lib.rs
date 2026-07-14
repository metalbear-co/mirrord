#![cfg(target_os = "linux")]
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
    remote_layer::load(&remote_layer_binary)
}

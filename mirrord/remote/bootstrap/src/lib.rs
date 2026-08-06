#![cfg(target_os = "linux")]
use ctor::ctor;
use mirrord_layer_lib::logging::init_tracing;

use crate::error::Result;

mod agent;
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
    agent::spawn(&agent_binary)
}

#![cfg(target_os = "linux")]
use std::{
    net::{SocketAddr, TcpStream},
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use ctor::ctor;
use mirrord_config::{MIRRORD_AGENT_SIDECAR_ADDR, MIRRORD_LAYER_INTPROXY_ADDR};
use mirrord_layer_lib::logging::init_tracing;

mod extract;

const DEFAULT_AGENT_SIDECAR_ADDR: &str = "127.0.0.1:61337";
const DEFAULT_INTPROXY_ADDR: &str = "127.0.0.1:61338";

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

fn spawn_remote_flow() -> Result<(), String> {
    let agent_addr = configured_addr(MIRRORD_AGENT_SIDECAR_ADDR, DEFAULT_AGENT_SIDECAR_ADDR)?;
    let intproxy_addr = configured_addr(MIRRORD_LAYER_INTPROXY_ADDR, DEFAULT_INTPROXY_ADDR)?;

    unsafe {
        std::env::set_var(MIRRORD_AGENT_SIDECAR_ADDR, agent_addr.to_string());
        std::env::set_var(MIRRORD_LAYER_INTPROXY_ADDR, intproxy_addr.to_string());
    }

    let agent_binary = extract::extract_agent_binary()?;
    let intproxy_binary = extract::extract_intproxy_remote_binary()?;

    tracing::info!(agent_binary = %agent_binary.display(), %agent_addr, "Launching sidecar agent");
    spawn_agent(&agent_binary)?;
    wait_for_endpoint(agent_addr, "agent-sidecar")?;

    tracing::info!(intproxy_binary = %intproxy_binary.display(), %intproxy_addr, "Launching remote intproxy");
    spawn_intproxy(&intproxy_binary, agent_addr)?;
    wait_for_endpoint(intproxy_addr, "remote intproxy")?;

    Ok(())
}

fn configured_addr(env_name: &str, default: &str) -> Result<SocketAddr, String> {
    let value = std::env::var(env_name).unwrap_or_else(|_| default.to_owned());
    value.parse().map_err(|error| {
        format!("failed parsing {env_name} value {value:?} as socket address: {error}")
    })
}

fn spawn_agent(binary: &std::path::Path) -> Result<(), String> {
    let mut command = Command::new(binary);
    command
        .arg("sidecar")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let child = command
        .spawn()
        .map_err(|error| format!("failed spawning sidecar process: {error}"))?;

    tracing::info!(pid = child.id(), "Spawned sidecar agent");
    Ok(())
}

fn spawn_intproxy(binary: &std::path::Path, agent_addr: SocketAddr) -> Result<(), String> {
    let mut command = Command::new(binary);
    command
        .env(MIRRORD_AGENT_SIDECAR_ADDR, agent_addr.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let child = command
        .spawn()
        .map_err(|error| format!("failed spawning remote intproxy process: {error}"))?;

    tracing::info!(pid = child.id(), "Spawned remote intproxy");
    Ok(())
}

fn wait_for_endpoint(address: SocketAddr, label: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        match TcpStream::connect_timeout(&address, Duration::from_millis(250)) {
            Ok(stream) => {
                drop(stream);
                tracing::debug!(%address, %label, "Endpoint is ready");
                return Ok(());
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for {label} at {address}: {error}"
                    ));
                }

                sleep(Duration::from_millis(100));
            }
        }
    }
}

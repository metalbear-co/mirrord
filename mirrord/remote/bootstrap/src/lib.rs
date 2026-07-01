#![cfg(target_os = "linux")]
use std::{
    ffi::{CStr, CString},
    os::unix::ffi::OsStrExt,
    path::Path,
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use ctor::ctor;
use mirrord_config::MIRRORD_AGENT_SIDECAR_REMOTE_ACCEPT_SOCKET;
use mirrord_layer_lib::logging::init_tracing;
use mirrord_remote_layer_protocol::{
    CONNECTION_HANDOFF_SOCKET_ENV, DEFAULT_CONNECTION_HANDOFF_SOCKET,
};

/// Environment variable used by the layer and sidecar to exchange accepted sockets over the
/// connection handoff socket.
const DEFAULT_CONNECTION_HANDOFF_SOCKET: &str = "/tmp/mirrord-remote-handoff.sock";

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

fn spawn_remote_flow() -> Result<(), String> {
    let connection_handoff_socket = std::env::var(CONNECTION_HANDOFF_SOCKET_ENV)
        .unwrap_or_else(|| DEFAULT_CONNECTION_HANDOFF_SOCKET.to_owned());
    let agent_binary = extract::extract_agent_binary()?;

    tracing::info!(agent_binary = %agent_binary.display(), %connection_handoff_socket, "Launching sidecar agent");
    spawn_agent(&agent_binary)?;
    wait_for_file(Path::new(&connection_handoff_socket))?;

    let remote_layer_binary = extract::extract_remote_layer_binary()?;
    load_remote_layer(&remote_layer_binary)?;

    Ok(())
}

fn load_remote_layer(binary: &std::path::Path) -> Result<(), String> {
    let binary_display = binary.display().to_string();
    tracing::info!(remote_layer_binary = %binary_display, "Loading remote layer");

    let binary = CString::new(binary.as_os_str().as_bytes()).map_err(|error| {
        format!("failed converting remote layer path {binary_display} to C string: {error}")
    })?;

    let handle = unsafe { libc::dlopen(binary.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        let error = unsafe {
            let error = libc::dlerror();
            if error.is_null() {
                "unknown dlopen error".to_owned()
            } else {
                CStr::from_ptr(error).to_string_lossy().into_owned()
            }
        };

        return Err(format!("failed loading remote layer: {error}"));
    }

    Ok(())
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

fn wait_for_file(path: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    while Instant::now() < deadline {
        if path.exists() {
            tracing::debug!(file = %path.display(), "File is ready");
            return Ok(());
        }

        sleep(Duration::from_millis(100));
    }

    Err(format!("timed out waiting for file at {}", path.display()))
}

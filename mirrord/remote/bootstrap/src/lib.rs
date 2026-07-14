#![cfg(target_os = "linux")]
use std::{
    ffi::{CStr, CString},
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use ctor::ctor;
use mirrord_layer_lib::logging::init_tracing;
use mirrord_remote_layer_protocol::connection_handoff::prepare_handoff_socket;

use crate::error::{RemoteBootstrapError, Result};

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
    let connection_handoff_socket = prepare_handoff_socket()?;
    let agent_binary = extract::extract_agent_binary()?;

    tracing::info!(agent_binary = %agent_binary.display(), ?connection_handoff_socket, "Launching sidecar agent");

    spawn_agent(&agent_binary)?;
    wait_for_file(&connection_handoff_socket)?;

    let remote_layer_binary = extract::extract_remote_layer_binary()?;
    load_remote_layer(&remote_layer_binary)?;

    Ok(())
}

fn load_remote_layer(binary: &Path) -> Result<()> {
    let binary_display = binary.display().to_string();
    tracing::info!(remote_layer_binary = %binary_display, "Loading remote layer");

    let binary = CString::new(binary.as_os_str().as_bytes())?;
    let handle = unsafe { libc::dlopen(binary.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        let error = unsafe { libc::dlerror() };
        let err_msg = {
            if error.is_null() {
                "unknown dlopen error".to_string()
            } else {
                unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() }
            }
        };
        return Err(RemoteBootstrapError::LayerLoad(err_msg));
    };

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

fn wait_for_file(path_buf: &PathBuf) -> Result<()> {
    let duration = Duration::from_secs(30);
    let deadline = Instant::now() + duration;

    let path = Path::new(&path_buf);
    while Instant::now() < deadline {
        if path.exists() {
            tracing::debug!(file = %path.display(), "File is ready");
            return Ok(());
        }

        sleep(Duration::from_millis(100));
    }

    Err(RemoteBootstrapError::AgentTimeout(
        path_buf.to_owned(),
        duration.as_secs(),
    ))
}

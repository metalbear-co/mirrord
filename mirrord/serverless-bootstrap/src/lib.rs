#![cfg_attr(not(target_os = "linux"), allow(unused))]

#[cfg(not(target_os = "linux"))]
compile_error!("mirrord-layer-bootstrap is supported only on Linux");

use std::{
    collections::BTreeMap,
    io::BufRead,
    net::{SocketAddr, TcpStream},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Command, Stdio},
};

use ctor::ctor;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream as TokioTcpStream,
};

const SESSION_ID_ENV: &str = "MIRRORD_SESSION_ID";

const DEFAULT_AGENT_BINARY: &str = "mirrord-agent";

#[ctor]
fn mirrord_layer_bootstrap_entry_point() {
    // make sure no other process loads bootstrap
    unsafe { std::env::remove_var("LD_PRELOAD") };

    let _ = spawn_agent();
}

fn spawn_agent() -> Result<(), String> {
    let mut command = Command::new(DEFAULT_AGENT_BINARY);
    command
        .arg("sidecar")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    command
        .spawn()
        .map_err(|error| format!("failed spawning sidecar process: {error}"))?;

    Ok(())
}

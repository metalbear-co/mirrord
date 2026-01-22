#![feature(try_blocks)]
#![feature(error_reporter)]
#![feature(try_with_capacity)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

#[cfg(target_os = "linux")]
use std::process::ExitCode;

#[cfg(target_os = "linux")]
use crate::{entrypoint::IPTABLES_DIRTY_EXIT_CODE, error::AgentError};

#[cfg(target_os = "linux")]
mod cli;
#[cfg(target_os = "linux")]
mod client_connection;
#[cfg(target_os = "linux")]
mod container_handle;
#[cfg(target_os = "linux")]
mod dns;
#[cfg(target_os = "linux")]
mod entrypoint;
#[cfg(target_os = "linux")]
mod env;
#[cfg(target_os = "linux")]
mod error;
#[cfg(target_os = "linux")]
mod file;
#[cfg(target_os = "linux")]
mod http;
#[cfg(target_os = "linux")]
mod incoming;
#[cfg(target_os = "linux")]
mod metrics;
#[cfg(target_os = "linux")]
mod mirror;
#[cfg(target_os = "linux")]
mod namespace;
#[cfg(target_os = "linux")]
mod outgoing;
#[cfg(target_os = "linux")]
mod reverse_dns;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod steal;
#[cfg(target_os = "linux")]
mod task;
#[cfg(target_os = "linux")]
mod util;
#[cfg(target_os = "linux")]
mod vpn;

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match crate::entrypoint::main().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(AgentError::IPTablesDirty) => ExitCode::from(IPTABLES_DIRTY_EXIT_CODE),
        _ => ExitCode::FAILURE,
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // mirrord agent is Linux-specific and does nothing on other platforms
    println!("mirrord-agent is only supported on Linux");
    std::process::exit(1);
}

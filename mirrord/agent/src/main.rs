#![cfg(target_os = "linux")]
#![feature(hash_extract_if)]
#![feature(let_chains)]
#![feature(iterator_try_collect)]
#![feature(try_blocks)]
#![feature(tcp_quickack)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use std::process::ExitCode;

use crate::error::AgentError;

mod cli;
mod client_connection;
mod container_handle;
mod dns;
mod entrypoint;
mod env;
mod error;
mod file;
mod http;
mod incoming;
mod metrics;
mod namespace;
mod outgoing;
mod runtime;
mod sniffer;
mod steal;
mod util;
mod vpn;

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match crate::entrypoint::main().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(AgentError::IPTablesDirty) => ExitCode::from(99),
        _ => ExitCode::FAILURE,
    }
}

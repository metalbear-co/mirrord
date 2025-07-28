#![cfg(target_os = "linux")]
#![feature(try_blocks)]
#![feature(error_reporter)]
#![feature(try_with_capacity)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

// TODO(alex): It's coming from our `fancy_regex::Error` conversion.
#![allow(clippy::result_large_err)]

use std::process::ExitCode;

use crate::{entrypoint::IPTABLES_DIRTY_EXIT_CODE, error::AgentError};

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
mod mirror;
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
        Err(AgentError::IPTablesDirty) => ExitCode::from(IPTABLES_DIRTY_EXIT_CODE),
        _ => ExitCode::FAILURE,
    }
}

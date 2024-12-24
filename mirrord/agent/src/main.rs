#![feature(hash_extract_if)]
#![feature(let_chains)]
#![feature(iterator_try_collect)]
#![feature(try_blocks)]
#![cfg_attr(target_os = "linux", feature(tcp_quickack))]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

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
mod namespace;
#[cfg(target_os = "linux")]
mod outgoing;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod sniffer;
#[cfg(target_os = "linux")]
mod steal;
#[cfg(target_os = "linux")]
mod util;
#[cfg(target_os = "linux")]
mod vpn;
#[cfg(target_os = "linux")]
mod watched_task;

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> crate::error::Result<()> {
    crate::entrypoint::main().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("This program is only supported on Linux");
}

/// To silence false positives from `deny(unused_crate_dependencies)`.
/// 
/// These dependencies are only used in integration tests.
#[cfg(all(test, target_os = "linux"))]
mod integration_tests_deps {
    use test_bin as _;
}

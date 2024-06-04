#![feature(hash_extract_if)]
#![feature(let_chains)]
#![feature(type_alias_impl_trait)]
#![cfg_attr(target_os = "linux", feature(tcp_quickack))]
#![feature(lazy_cell)]
#![warn(clippy::indexing_slicing)]

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

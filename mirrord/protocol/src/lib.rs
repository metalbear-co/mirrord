#![feature(const_trait_impl)]
#![feature(io_error_more)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

pub mod batched_body;
pub mod codec;
pub mod dns;
pub mod error;
pub mod file;
pub mod outgoing;
#[deprecated = "pause feature was removed"]
pub mod pause;
pub mod tcp;
pub mod vpn;

use std::{collections::HashSet, ops::Deref, sync::LazyLock};

pub use codec::*;
pub use error::*;

pub type Port = u16;
pub type ConnectionId = u64;

/// A per-connection HTTP request ID
pub type RequestId = u16; // TODO: how many requests in a single connection? is u16 appropriate?

pub static VERSION: LazyLock<semver::Version> = LazyLock::new(|| {
    env!("CARGO_PKG_VERSION")
        .parse()
        .expect("Bad version parsing")
});

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvVars(pub String);

impl From<EnvVars> for HashSet<String> {
    fn from(env_vars: EnvVars) -> Self {
        env_vars
            .split_terminator(';')
            .map(String::from)
            .collect::<HashSet<_>>()
    }
}

impl Deref for EnvVars {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

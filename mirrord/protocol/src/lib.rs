#![feature(const_trait_impl)]
#![feature(io_error_more)]
#![feature(lazy_cell)]
#![feature(result_option_inspect)]
#![warn(clippy::indexing_slicing)]

pub mod codec;
pub mod dns;
pub mod error;
pub mod file;
pub mod outgoing;
pub mod pause;
pub mod tcp;

use core::fmt;
use std::{collections::HashSet, ops::Deref, str::FromStr, sync::LazyLock};

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

/// Used to identify if the remote pod is in a mesh context.
///
/// - Passed to the agent so we can handle the special case for selecting a network interface `lo`
///   when we're mirroring with `istio`;
/// - Used in the stealer iptables handling to add/detect special rules for meshes;
///
/// ## Internal
///
/// Can be converted to, and from `String`, if you add a new value here, don't forget to add it to
/// the `FromStr` implementation!
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MeshVendor {
    Linkerd,
    Istio,
}

impl fmt::Display for MeshVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeshVendor::Linkerd => write!(f, "linkerd"),
            MeshVendor::Istio => write!(f, "istio"),
        }
    }
}

impl FromStr for MeshVendor {
    type Err = MeshVendorParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "linkerd" => Ok(Self::Linkerd),
            "istio" => Ok(Self::Istio),
            invalid => Err(MeshVendorParseError(invalid.into())),
        }
    }
}

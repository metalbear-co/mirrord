#![feature(const_trait_impl)]
#![feature(io_error_more)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

pub mod body_chunks;
pub mod codec;
pub mod dns;
pub mod error;
pub mod file;
pub mod outgoing;
pub mod pause;
pub mod tcp;
pub mod vpn;

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
    Kuma,
    IstioAmbient,
    IstioCni,
}

impl fmt::Display for MeshVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeshVendor::Linkerd => write!(f, "linkerd"),
            MeshVendor::Istio => write!(f, "istio"),
            MeshVendor::Kuma => write!(f, "kuma"),
            MeshVendor::IstioAmbient => write!(f, "istio-ambient"),
            MeshVendor::IstioCni => write!(f, "istio-cni"),
        }
    }
}

impl FromStr for MeshVendor {
    type Err = MeshVendorParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "linkerd" => Ok(Self::Linkerd),
            "istio" => Ok(Self::Istio),
            "kuma" => Ok(Self::Kuma),
            "istio-ambient" => Ok(Self::IstioAmbient),
            invalid => Err(MeshVendorParseError(invalid.into())),
        }
    }
}

/// Name of environment variable
///
/// Name of environment variable that can be used to provide the agent with a PEM-encoded X509
/// certificate. Given the certificate, the agent will secure the incoming connections with TLS.
/// The agent will act as TLS client and will make successful connections only with TLS servers
/// using the given certificate. Regardless of this setting, the agent will still be the side that
/// accepts the initial TCP connection.
///
/// # Note
///
/// This may not be the best place to put this name, but this is the only crate shared by
/// `mirrord-kube` and `mirrord-agent`.
pub const AGENT_OPERATOR_CERT_ENV: &str = "MIRRORD_AGENT_OPERATOR_CERT";

pub const AGENT_NETWORK_INTERFACE_ENV: &str = "MIRRORD_AGENT_INTERFACE";

pub const AGENT_IPV6_ENV: &str = "MIRRORD_AGENT_SUPPORT_IPV6";

use std::{fmt, str::FromStr};

use thiserror::Error;

/// Used to identify if the target pod is in a mesh context.
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

/// Error when parsing a [`MeshVendor`] from the agent arguments.
#[derive(Debug, PartialEq, Clone, Eq, Error)]
#[error("unknown variant `{0}`")]
pub struct MeshVendorParseError(pub String);

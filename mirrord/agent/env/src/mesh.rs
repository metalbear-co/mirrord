use std::fmt;

/// Type of a mesh context in which the target pod runs.
///
/// Used to be detected in the CLI and passed to the agent in a command line argument.
/// Now the agent independently infers it from the iptables.
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

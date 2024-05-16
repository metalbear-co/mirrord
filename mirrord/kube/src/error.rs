use std::net::AddrParseError;

use thiserror::Error;

pub type Result<T, E = KubeApiError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum KubeApiError {
    #[error("Kube failed: {0}")]
    KubeError(#[from] kube::Error),

    #[error("Connection to agent failed: {0}")]
    KubeConnectionError(#[from] std::io::Error),

    #[error("Failed to infer Kube config: {0}")]
    InferKubeConfigError(#[from] kube::config::InferConfigError),

    #[error("Failed to load Kube config: {0}")]
    KubeConfigPathError(#[from] kube::config::KubeconfigError),

    #[error("Invalid target provided: {0}")]
    InvalidTarget(String),

    #[error("Failed to get `Spec` for Pod!")]
    PodSpecNotFound,

    #[error("Deployment `{0}` not found")]
    DeploymentNotFound(String),

    #[error("Failed to extract container runtime from container id `{0}`")]
    ContainerRuntimeParseError(String),

    #[error("Container ID not found in response from kube API")]
    ContainerIdNotFound,

    #[error("Failed to get Pod for Job `{0}`!")]
    JobPodNotFound(String),

    #[error("Pod name not found in response from kube API")]
    PodNameNotFound,

    #[error("Node name wasn't found in pod spec")]
    NodeNotFound,

    #[error("Pod status not found in response from kube API")]
    PodStatusNotFound,

    #[error("Container status not found in response from kube API")]
    ContainerStatusNotFound,

    #[error("Container not found: `{0}`")]
    ContainerNotFound(String),

    #[error("Timeout waiting for agent to be ready")]
    AgentReadyTimeout,

    #[error("Port not found in port forward")]
    PortForwardFailed,

    #[error("Invaild Address Conversion: {0}")]
    InvalidAddress(#[from] AddrParseError),

    /// This error should never happen, but has to exist if we don't want to unwrap.
    #[error("None runtime data for non-targetless agent. This is a bug.")]
    MissingRuntimeData,

    #[error("Failed to load incluster Kube config: {0}")]
    KubeInclusterError(#[from] kube::config::InClusterError),

    #[error("Unable to fetch node limits for node `{0}`")]
    NodeBadAllocatable(String),

    #[error("Node `{0}` is too full with {1} pods")]
    NodePodLimitExceeded(String, usize),

    #[error("Path expansion for kubeconfig failed: {0}")]
    ConfigPathExpansionError(String),
}

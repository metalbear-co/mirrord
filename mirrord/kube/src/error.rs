use std::net::AddrParseError;

use thiserror::Error;

pub type Result<T, E = KubeApiError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum KubeApiError {
    #[error("mirrord-layer: Kube failed with error `{0}`!")]
    KubeError(#[from] kube::Error),

    #[error("mirrord-layer: Connection to agent failed `{0}`!")]
    KubeConnectionError(#[from] std::io::Error),

    #[error("mirrord-layer: Failed to get `KubeConfig`!")]
    KubeConfigError(#[from] kube::config::InferConfigError),

    #[error("mirrord-layer: Failed to get the custom path for `KubeConfig`!")]
    KubeConfigPathError(#[from] kube::config::KubeconfigError),

    #[error("mirrord-layer: JSON convert error")]
    JSONConvertError(#[from] serde_json::Error),

    #[error("mirrord-layer: Invalid target proivded `{0}`!")]
    InvalidTarget(String),

    #[error("mirrord-layer: Failed to get `Spec` for Pod!")]
    PodSpecNotFound,

    #[error("mirrord-layer: Deployment: `{0} not found!`")]
    DeploymentNotFound(String),

    #[error("mirrord-layer: Failed to get Container runtime data for `{0}`!")]
    ContainerRuntimeParseError(String),

    #[error("mirrord-layer: Container ID not found in response from kube API")]
    ContainerIdNotFound,

    #[error("mirrord-layer: Failed to get Pod for Job `{0}`!")]
    JobPodNotFound(String),

    #[error("mirrord-layer: Pod name not found in response from kube API")]
    PodNameNotFound,

    #[error("mirrord-layer: Node name wasn't found in pod spec")]
    NodeNotFound,

    #[error("mirrord-layer: Pod status not found in response from kube API")]
    PodStatusNotFound,

    #[error("mirrord-layer: Container status not found in response from kube API")]
    ContainerStatusNotFound,

    #[error("mirrord-layer: Container not found: `{0}`")]
    ContainerNotFound(String),

    #[error("mirrord-layer: Timeout waiting for agent to be ready")]
    AgentReadyTimeout,

    #[error("Port not found in port forward")]
    PortForwardFailed,

    #[error("Invaild Address Conversion: {0}")]
    InvalidAddress(#[from] AddrParseError),

    /// This error should never happen, but has to exist if we don't want to unwrap.
    #[error("mirrord-layer: None runtime data for non-targetless agent. This is a bug.")]
    MissingRuntimeData,
}

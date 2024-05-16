use std::fmt;

use kube::Resource;
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

    // #[error("Failed to extract container runtime from container `{}` (`id {}`)", .name, .id)]
    // ContainerRuntimeParseError { name: String, id: String },
    #[error("Timeout waiting for agent to be ready")]
    AgentReadyTimeout,

    #[error("Port not found in port forward")]
    PortForwardFailed,

    /// This error should never happen, but has to exist if we don't want to unwrap.
    #[error("None runtime data for non-targetless agent. This is a bug.")]
    MissingRuntimeData,

    #[error("Failed to load incluster Kube config: {0}")]
    KubeInclusterError(#[from] kube::config::InClusterError),

    #[error("Path expansion for kubeconfig failed: {0}")]
    ConfigPathExpansionError(String),

    /// We fetched a malformed resource using [`kube`] (should not happen).
    /// Construct with [`Self::missing_field`] or [`Self::invalid_value`] for consistent error
    /// messages.
    #[error("Kube API returned a malformed resource: {0}")]
    MalformedResource(String),

    /// Resource fetched using [`kube`] is in invalid state.
    /// Construct with [`Self::invalid_state`] for consistent error messages.
    #[error("Resource fetched from Kube API is invalid: {0}")]
    InvalidResourceState(String),

    #[error("Agent Job was created, but Pod is not running")]
    AgentPodNotRunning,
}

impl KubeApiError {
    pub fn missing_field<R: Resource<DynamicType = ()>>(resource: &R, field_name: &str) -> Self {
        let kind = R::kind(&());
        let name = resource.meta().name.as_ref();
        let namespace = resource.meta().namespace.as_deref().unwrap_or("default");

        let message = match name {
            Some(name) => format!("{kind} `{namespace}/{name}` is missing field `{field_name}`"),
            None => format!("{kind} is missing field `{field_name}`"),
        };

        Self::MalformedResource(message)
    }

    pub fn invalid_value<R: Resource<DynamicType = ()>, T: fmt::Display>(
        resource: &R,
        field_name: &str,
        info: T,
    ) -> Self {
        let kind = R::kind(&());
        let name = resource.meta().name.as_ref();
        let namespace = resource.meta().namespace.as_deref().unwrap_or("default");

        let message = match name {
            Some(name) => {
                format!("field `{field_name}` in {kind} `{namespace}/{name}` is invalid: {info}")
            }
            None => format!("field `{field_name}` in a {kind} is invalid: {info}"),
        };

        Self::MalformedResource(message)
    }

    pub fn invalid_state<R: Resource<DynamicType = ()>, T: fmt::Display>(
        resource: &R,
        info: T,
    ) -> Self {
        let kind = R::kind(&());
        let name = resource.meta().name.as_ref();
        let namespace = resource.meta().namespace.as_deref().unwrap_or("default");

        let message = match name {
            Some(name) => {
                format!("{kind} `{namespace}/{name}` is in invalid state: {info}")
            }
            None => format!("{kind} is in invalid state: {info}"),
        };

        Self::MalformedResource(message)
    }
}

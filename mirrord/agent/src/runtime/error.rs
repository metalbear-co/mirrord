use thiserror::Error;

/// Errors that can occur when fetching target container info.
#[derive(Error, Debug)]
pub(crate) enum ContainerRuntimeError {
    #[error("failed to get target container info: {} [{}]", .error, .runtime)]
    GetInfoError { runtime: String, error: String },
    #[error("unknown container runtime `{0}`")]
    UnknownRuntimeName(String),
}

impl ContainerRuntimeError {
    pub(crate) fn crio<E: ToString>(error: E) -> Self {
        Self::GetInfoError {
            runtime: "cri-o".into(),
            error: error.to_string(),
        }
    }

    pub(crate) fn docker<E: ToString>(error: E) -> Self {
        Self::GetInfoError {
            runtime: "docker".into(),
            error: error.to_string(),
        }
    }

    pub(crate) fn containerd<E: ToString>(error: E) -> Self {
        Self::GetInfoError {
            runtime: "containerd".into(),
            error: error.to_string(),
        }
    }

    pub(crate) fn unknown_runtime<N: ToString>(name: N) -> Self {
        Self::UnknownRuntimeName(name.to_string())
    }
}

pub(crate) type Result<T, E = ContainerRuntimeError> = core::result::Result<T, E>;

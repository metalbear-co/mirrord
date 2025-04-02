use std::io;

use thiserror::Error;

use crate::namespace::NamespaceError;

/// Errors that can occur when creating a [`RemoteRuntime`](super::remote_runtime::RemoteRuntime).
#[derive(Error, Debug)]
pub enum RemoteRuntimeError {
    #[error("failed to spawn runtime thread: {0}")]
    ThreadSpawnError(#[source] io::Error),
    #[error(transparent)]
    NamespaceError(#[from] NamespaceError),
    #[error("failed to build tokio runtime: {0}")]
    TokioRuntimeError(#[source] io::Error),
    #[error("runtime thread panicked")]
    Panicked,
}

/// An error that occurs when polling a future spawned with
/// [`RemoteRuntime::spawn`](super::remote_runtime::RemoteRuntime::spawn) or
/// [`MaybeRemoteRuntime::spawn`](super::remote_runtime::MaybeRemoteRuntime::spawn).
///
/// This error indicated that the future has panicked.
#[derive(Debug, Error)]
#[error("task panicked")]
pub struct BgTaskPanicked;

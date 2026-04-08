use std::sync::Arc;

use mirrord_protocol::{DaemonMessage, ResponseError};
use thiserror::Error;
use tokio::task::JoinError;

use crate::timeout::Elapsed;

/// [`MirrordClient`](crate::client::MirrordClient) errors.
#[derive(Error, Debug, Clone)]
pub enum ClientError {
    /// The background task failed. The client is no longer usable.
    #[error("client task failed")]
    TaskFailed(#[source] TaskError),
    /// The background task lost the connection to the server.
    ///
    /// The task might still reconnect, but the connection state was lost.
    /// Ongoing operations were interrupted.
    #[error("connection was lost")]
    ConnectionLost(#[source] TaskError),
    /// The operation failed in the server.
    #[error("request failed")]
    Response(#[from] ResponseError),
    /// The server does not support the requested operation due to its [`mirrord_protocol`] version.
    #[error("server protocol version does not support the operation")]
    NotSupported,
    /// Remote file was lost with the previous server connection.
    #[error(
        "request refers to a file descriptor {0} that was lost after reconnecting to the server"
    )]
    LostFileDescriptor(u64),
}

pub type ClientResult<T, E = ClientError> = Result<T, E>;

/// Errors that can occur in the background task powering
/// [`MirrordClient`](crate::client::MirrordClient)s.
#[derive(Error, Debug, Clone)]
pub enum TaskError {
    /// Connection with the server failed.
    #[error("IO on server connection failed")]
    Io(#[source] Arc<dyn 'static + std::error::Error + Send + Sync>),
    /// Server closed the connection.
    #[error("server closed connection: {}", .0.as_deref().unwrap_or("<no reason given>"))]
    ServerClosed(
        /// Taken from [`DaemonMessage::Close`] if one was received.
        Option<String>,
    ),
    /// The server failed to respond to our ping in time.
    #[error("server failed to send pong in time")]
    MissedPing,
    /// The task reconnected to the server, but the server's protocol version was downgraded.
    ///
    /// [`MirrordClient`](crate::client::MirrordClient) does not handle this case to simplify the
    /// API. This error should be very rare (only when the operator is downgraded).
    #[error(
        "reconnected to the server with downgraded protocol version: initial={}, downgraded={}",
        .0.0,
        .0.1,
    )]
    ReconnectedWithDowngradedProtocol(Arc<(semver::Version, semver::Version)>),
    /// The server violated the [`mirrord_protocol`] in some way,
    /// for example, sent an unexpected message.
    #[error("server violated the protocol: {0}")]
    ProtocolViolation(Arc<str>),
    /// The task was canceled or panicked.
    #[error(transparent)]
    JoinError(#[from] Arc<JoinError>),
    /// Time limit configured in [`crate::client::ClientConfig`] elapsed.
    #[error("exceeded configured communication time limit")]
    CommunicationTimeout(#[from] Elapsed),
    #[error("finished unexpectedly without error")]
    UnexpectedlyFinished,
}

impl TaskError {
    pub(super) fn io<E: 'static + std::error::Error + Send + Sync>(error: E) -> Self {
        Self::Io(Arc::new(error))
    }

    pub(super) fn unexpected_message(message: &DaemonMessage) -> Self {
        let as_string = format!("sent an unexpected message: {message:?}");
        Self::ProtocolViolation(Arc::from(as_string))
    }

    pub(super) fn can_reconnect(&self) -> bool {
        match self {
            Self::Io(..)
            | Self::ServerClosed(..)
            | Self::MissedPing
            | Self::CommunicationTimeout { .. } => true,
            Self::ReconnectedWithDowngradedProtocol { .. }
            | Self::ProtocolViolation(..)
            | Self::JoinError(..)
            | Self::UnexpectedlyFinished => false,
        }
    }
}

pub type TaskResult<T, E = TaskError> = Result<T, E>;

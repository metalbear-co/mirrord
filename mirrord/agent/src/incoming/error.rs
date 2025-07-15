use std::{error::Error, io, sync::Arc};

use thiserror::Error;

use super::tls::error::StealTlsSetupError;

/// Errors that can occur in the [`RedirectorTask`](super::RedirectorTask).
#[derive(Error, Debug, Clone)]
pub enum RedirectorTaskError {
    /// A runtime error that occurred in the task.
    ///
    /// For simplicity, the inner error is opaque.
    /// Any redirector task error should terminate the agent,
    /// so we're not really interested in the concrete type.
    #[error("port redirector failed: {0}")]
    RedirectorError(Arc<dyn Error + Send + Sync + 'static>),
    /// The task panicked.
    #[error("port redirector task panicked")]
    Panicked,
}

/// Errors that can occur when detecting HTTP protocol in a redirected connection.
#[derive(Error, Debug)]
pub enum HttpDetectError {
    #[error(transparent)]
    TlsSetup(#[from] StealTlsSetupError),
    #[error("failed to get the local address of the connection: {0}")]
    LocalAddr(#[source] io::Error),
    #[error("connection failed during HTTP detection: {0}")]
    HttpDetect(#[source] io::Error),
    #[error("failed to accept the TLS connection: {0}")]
    TlsAccept(#[source] io::Error),
}

/// Errors that can occur when handling a redirected incoming connection.
#[derive(Error, Debug, Clone)]
pub enum ConnError {
    #[error("failed to make a passthrough TCP connection: {0}")]
    TcpConnectError(#[source] Arc<io::Error>),
    #[error("failed to make a passthrough TLS connection: {0}")]
    TlsConnectError(#[source] Arc<io::Error>),
    #[error("incoming TCP connection failed: {0}")]
    IncomingTcpError(#[source] Arc<io::Error>),
    #[error("passthrough TCP connection failed: {0}")]
    PassthroughTcpError(#[source] Arc<io::Error>),
    #[error("incoming HTTP connection failed: {0}")]
    IncomingHttpError(#[source] Arc<hyper::Error>),
    #[error("passthrough HTTP connection failed: {0}")]
    PassthroughHttpError(#[source] Arc<hyper::Error>),
    #[error("upgraded HTTP connection failed: {0}")]
    UpgradedError(#[source] Arc<io::Error>),
    #[error("stealing client dropped the connection/request")]
    StealerDropped,
    #[error("connection task was dropped")]
    Dropped,
    #[error("broadcast receiver lagged begind")]
    BroadcastLag,
}

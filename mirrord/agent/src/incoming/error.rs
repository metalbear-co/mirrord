use std::{error::Error, io, sync::Arc};

use thiserror::Error;

use super::tls::error::IncomingTlsSetupError;

#[derive(Error, Debug, Clone, Copy)]
#[error("stealing client dropped the connection/request")]
pub struct StealerDropped;

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
    #[error("redirector task panicked")]
    Panicked,
}

/// Errors that can occur when detecting HTTP protocol in a redirected connection.
#[derive(Error, Debug)]
pub enum HttpDetectError {
    #[error(transparent)]
    TlsSetup(#[from] IncomingTlsSetupError),
    #[error("failed to get local address of the connection: {0}")]
    LocalAddr(#[source] io::Error),
    #[error("connection failed during HTTP detection: {0}")]
    HttpDetect(#[source] io::Error),
    #[error("failed to accept TLS connection: {0}")]
    TlsAccept(#[source] io::Error),
}

/// Errors that can occur when handling a redirected incoming connection.
///
/// Most inner errors are wrapped in [`Arc`], as this type must implement [`Clone`] - errors are
/// broadcasted to all interested clients.
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
    #[error("upgraded incoming connection failed: {0}")]
    IncomingUpgradedError(#[source] Arc<io::Error>),
    #[error("upgraded passthrough connection failed: {0}")]
    PassthroughUpgradedError(#[source] Arc<io::Error>),
    #[error("client was not able to process mirrored incoming data on time")]
    Lagged,
    #[error(transparent)]
    StealerDropped(#[from] StealerDropped),
    #[error("connection task panicked")]
    Panicked,
}

/// Convenience trait for converting results to [`Result<T, ConnError>`].
///
/// As most inner errors in [`ConnError`] are wrapped in [`Arc`],
/// single [`ResultExt::map_err_into`] call can be used instead of a chain like:
///
/// ```rust,no-run
/// result
///     .map_err(Arc::new)
///     .map_err(ConnError::TcpConnectError)
/// ```
pub trait ResultExt {
    type Ok;
    type Err;

    fn map_err_into<F, E1, E2>(self, f: F) -> Result<Self::Ok, E2>
    where
        F: FnOnce(E1) -> E2,
        E1: From<Self::Err>;
}

impl<T, E> ResultExt for Result<T, E> {
    type Ok = T;
    type Err = E;

    fn map_err_into<F, E1, E2>(self, f: F) -> Result<T, E2>
    where
        F: FnOnce(E1) -> E2,
        E1: From<E>,
    {
        self.map_err(E1::from).map_err(f)
    }
}

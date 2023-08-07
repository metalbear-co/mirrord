use mirrord_protocol::ConnectionId;
use thiserror::Error;

use crate::steal::HandlerHttpRequest;

/// Errors specific to the HTTP traffic feature.
#[derive(Error, Debug)]
pub enum HttpTrafficError {
    #[error("Failed with IO `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("Failed with Parse `{0}`!")]
    Parse(#[from] httparse::Error),

    #[error("Failed with Hyper `{0}`!")]
    Hyper(#[from] hyper::Error),

    #[error("Failed with Captured `{0}`!")]
    MatchedSender(#[from] tokio::sync::mpsc::error::SendError<HandlerHttpRequest>),

    #[error("Failed with Captured `{0}`!")]
    ResponseReceiver(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("Failed hyper HTTP `{0}`!")]
    HyperHttp(#[from] hyper::http::Error),

    #[error("Failed closing connection with `{0}`!")]
    CloseSender(#[from] tokio::sync::mpsc::error::SendError<ConnectionId>),

    #[error(transparent)]
    Never(#[from] std::convert::Infallible),
}

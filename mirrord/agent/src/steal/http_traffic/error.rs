use thiserror::Error;

use super::MatchedHttpRequest;
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
}

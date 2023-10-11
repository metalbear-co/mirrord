use std::{io, net::SocketAddr};

use thiserror::Error;

use crate::{
    codec::CodecError,
    protocol::{LayerToProxyMessage, SessionId},
    session::ProxySessionError,
};

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("proxy session {0} task failed: {1}")]
    ProxySessionError(SessionId, ProxySessionError),
    #[error("proxy session {0} task panicked")]
    ProxySessionPanic(SessionId),
    #[error("waiting for the first layer connection timed out")]
    FirstConnectionTimeout,
    #[error("accepting layer connection failed: {0}")]
    AcceptFailed(io::Error),
    #[error("initializing layer session with {0} failed: {1}")]
    SessionInitError(SocketAddr, SessionInitError),
    #[error("layer sent unexpected message: {0:?}")]
    UnexpectedLayerMessage(LayerToProxyMessage),
}

#[derive(Error, Debug)]
pub enum SessionInitError {
    #[error("{0}")]
    Codec(#[from] CodecError),
    #[error("layer sent unexpected message {0:?}")]
    UnexpectedMessage(LayerToProxyMessage),
    #[error("{0}")]
    ProxySessionError(#[from] ProxySessionError),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;

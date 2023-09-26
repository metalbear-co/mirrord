use std::io;

use thiserror::Error;
use tokio::sync::mpsc::error::SendError;

use crate::{
    agent_conn::AgentCommunicationFailed,
    codec::CodecError,
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage},
};

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("waiting for the first layer connection timed out")]
    FirstConnectionTimeout,
    #[error("accepting layer connection failed: {0}")]
    AcceptFailed(io::Error),
    #[error("communication with agent failed: {0}")]
    AgentCommunicationFailed(#[from] AgentCommunicationFailed),
    #[error("communication with layer failed: {0}")]
    LayerCommunicationFailed(#[from] LayerCommunicationFailed),
}

#[derive(Error, Debug)]
pub enum LayerCommunicationFailed {
    #[error("response queue is empty")]
    RequestQueueEmpty,
    #[error("channel is closed")]
    ChannelClosed,
    #[error("binary protocol failed: {0}")]
    CodecFailed(#[from] CodecError),
    #[error("received unexpected message")]
    UnexpectedMessage(LayerToProxyMessage),
}

impl From<CodecError> for IntProxyError {
    fn from(value: CodecError) -> Self {
        Self::LayerCommunicationFailed(LayerCommunicationFailed::CodecFailed(value))
    }
}

impl From<SendError<LocalMessage<ProxyToLayerMessage>>> for LayerCommunicationFailed {
    fn from(_value: SendError<LocalMessage<ProxyToLayerMessage>>) -> Self {
        Self::ChannelClosed
    }
}

impl From<SendError<LocalMessage<ProxyToLayerMessage>>> for IntProxyError {
    fn from(value: SendError<LocalMessage<ProxyToLayerMessage>>) -> Self {
        Self::LayerCommunicationFailed(value.into())
    }
}

pub type Result<T> = core::result::Result<T, IntProxyError>;

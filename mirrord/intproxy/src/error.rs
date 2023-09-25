use std::io;

use mirrord_kube::error::KubeApiError;
use mirrord_operator::client::OperatorApiError;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use thiserror::Error;
use tokio::sync::mpsc::error::SendError;

use crate::{
    codec::CodecError,
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage},
};

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("waiting for the first layer connection timed out")]
    FirstConnectionTimeout,
    #[error("waiting for the first layer connection failed")]
    FirstAcceptFailed(io::Error),
    #[error("accepting layer connection failed: {0:?}")]
    AcceptFailed(io::Error),
    #[error("connecting to the agent failed: {0:?}")]
    RawAgentConnectionFailed(io::Error),
    #[error("connecting to the agent failed: {0:?}")]
    OperatorAgentConnectionFailed(OperatorApiError),
    #[error("connecting to the agent failed: {0:?}")]
    KubeApiAgentConnectionFailed(KubeApiError),
    #[error("could not find method for agent connection")]
    NoConnectionMethod,
    #[error("communication with agent failed: {0:?}")]
    AgentCommunicationFailed(#[from] AgentCommunicationFailed),
    #[error("communication with layer failed: {0:?}")]
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
    #[error("received unexpected message: {0:?}")]
    UnexpectedMessage(LayerToProxyMessage),
}

#[derive(Error, Debug)]
pub enum AgentCommunicationFailed {
    #[error("agent did not respond to ping message in time")]
    UnmatchedPing,
    #[error("channel is closed")]
    ChannelClosed,
    #[error("received unexpected message: {0:?}")]
    UnexpectedMessage(DaemonMessage),
}

impl From<CodecError> for IntProxyError {
    fn from(value: CodecError) -> Self {
        Self::LayerCommunicationFailed(LayerCommunicationFailed::CodecFailed(value))
    }
}

impl From<SendError<ClientMessage>> for AgentCommunicationFailed {
    fn from(_value: SendError<ClientMessage>) -> Self {
        Self::ChannelClosed
    }
}

impl From<SendError<ClientMessage>> for IntProxyError {
    fn from(value: SendError<ClientMessage>) -> Self {
        Self::AgentCommunicationFailed(value.into())
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

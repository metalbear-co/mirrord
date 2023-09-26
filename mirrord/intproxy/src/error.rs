use std::io;

use thiserror::Error;

use crate::{
    agent_conn::AgentCommunicationFailed, layer_conn::LayerCommunicationFailed,
    protocol::LayerToProxyMessage,
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
    #[error("request queue is empty")]
    RequestQueueEmpty,
    #[error("received unexpected message from layer")]
    UnexpectedLayerMessage(LayerToProxyMessage),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;

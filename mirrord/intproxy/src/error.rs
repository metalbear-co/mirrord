use std::io;

use mirrord_kube::error::KubeApiError;
use mirrord_operator::client::OperatorApiError;
use thiserror::Error;

use crate::codec::CodecError;

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
    #[error("communication with agent failed")]
    AgentCommunicationFailed,
    #[error("communication with layer failed: {0:?}")]
    CodecFailed(#[from] CodecError),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;

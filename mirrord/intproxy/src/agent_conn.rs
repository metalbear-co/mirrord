use std::io;

use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{
        kubernetes::{AgentKubernetesConnectInfo, KubernetesAPI},
        wrap_raw_connection, AgentManagment,
    },
    error::KubeApiError,
};
use mirrord_operator::client::{OperatorApi, OperatorApiError, OperatorSessionInformation};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};

#[derive(Error, Debug)]
pub enum AgentCommunicationError {
    #[error("channel closed")]
    ChannelClosed,
    #[error("received unexpected message: {0:?}")]
    UnexpectedMessage(DaemonMessage),
    #[error("failed to connect: {0}")]
    ConnectionError(#[from] AgentConnectionError),
}

#[derive(Error, Debug)]
pub enum AgentConnectionError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Operator(#[from] OperatorApiError),
    #[error("{0}")]
    Kube(#[from] KubeApiError),
    #[error("invalid configuration, could not find method for connection")]
    NoConnectionMethod,
}

pub type Result<T> = core::result::Result<T, AgentCommunicationError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentConnectInfo {
    /// Connect to the agent through the operator.
    Operator(OperatorSessionInformation),
    /// Connect directly to the agent by name and port using k8s port forward.
    DirectKubernetes(AgentKubernetesConnectInfo),
}

pub struct AgentConnection {
    sender: AgentSender,
    receiver: Receiver<DaemonMessage>,
}

impl AgentConnection {
    pub async fn new(
        config: &LayerConfig,
        connect_info: Option<&AgentConnectInfo>,
    ) -> Result<Self> {
        let (sender, receiver) = match connect_info.as_ref() {
            Some(AgentConnectInfo::Operator(operator_session_information)) => {
                OperatorApi::connect(config, operator_session_information, None)
                    .await
                    .map_err(AgentConnectionError::Operator)?
            }
            Some(AgentConnectInfo::DirectKubernetes(connect_info)) => {
                let k8s_api = KubernetesAPI::create(config)
                    .await
                    .map_err(AgentConnectionError::Kube)?;
                k8s_api
                    .create_connection(connect_info.clone())
                    .await
                    .map_err(AgentConnectionError::Kube)?
            }
            None => {
                if let Some(address) = config.connect_tcp.as_ref() {
                    let stream = TcpStream::connect(address)
                        .await
                        .map_err(AgentConnectionError::Io)?;

                    wrap_raw_connection(stream)
                } else {
                    return Err(AgentConnectionError::NoConnectionMethod.into());
                }
            }
        };

        Ok(AgentConnection {
            sender: AgentSender(sender),
            receiver,
        })
    }

    pub async fn ping_pong(&mut self) -> Result<()> {
        self.sender.send(ClientMessage::Ping).await?;

        let response = self
            .receiver
            .recv()
            .await
            .ok_or(AgentCommunicationError::ChannelClosed)?;

        match response {
            DaemonMessage::Pong => Ok(()),
            other => Err(AgentCommunicationError::UnexpectedMessage(other)),
        }
    }

    pub fn sender(&self) -> &AgentSender {
        &self.sender
    }

    pub async fn receive(&mut self) -> Result<DaemonMessage> {
        self.receiver
            .recv()
            .await
            .ok_or(AgentCommunicationError::ChannelClosed)
    }
}

#[derive(Clone)]
pub struct AgentSender(Sender<ClientMessage>);

impl AgentSender {
    pub async fn send(&self, message: ClientMessage) -> Result<()> {
        self.0
            .send(message)
            .await
            .map_err(|_| AgentCommunicationError::ChannelClosed)
    }
}

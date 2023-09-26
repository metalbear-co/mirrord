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
    sync::mpsc::{error::SendError, Receiver, Sender},
};

#[derive(Error, Debug)]
pub enum AgentCommunicationFailed {
    #[error("failed to connect: {0}")]
    ConnectionFailed(#[from] AgentConnectionFailed),
    #[error("agent did not respond to ping message in time")]
    UnmatchedPing,
    #[error("channel is closed")]
    ChannelClosed,
    #[error("received unexpected message: {0:?}")]
    UnexpectedMessage(DaemonMessage),
}

#[derive(Error, Debug)]
pub enum AgentConnectionFailed {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Operator(#[from] OperatorApiError),
    #[error("{0}")]
    Kube(#[from] KubeApiError),
    #[error("could not find method for agent connection")]
    NoConnectionMethod,
}

impl From<SendError<ClientMessage>> for AgentCommunicationFailed {
    fn from(_value: SendError<ClientMessage>) -> Self {
        Self::ChannelClosed
    }
}

pub type Result<T> = core::result::Result<T, AgentCommunicationFailed>;

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
                    .map_err(AgentConnectionFailed::Operator)?
            }
            Some(AgentConnectInfo::DirectKubernetes(connect_info)) => {
                let k8s_api = KubernetesAPI::create(config)
                    .await
                    .map_err(AgentConnectionFailed::Kube)?;
                k8s_api
                    .create_connection(connect_info.clone())
                    .await
                    .map_err(AgentConnectionFailed::Kube)?
            }
            None => {
                if let Some(address) = config.connect_tcp.as_ref() {
                    let stream = TcpStream::connect(address)
                        .await
                        .map_err(AgentConnectionFailed::Io)?;

                    wrap_raw_connection(stream)
                } else {
                    return Err(AgentConnectionFailed::NoConnectionMethod.into());
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
            .ok_or(AgentCommunicationFailed::ChannelClosed)?;

        match response {
            DaemonMessage::Pong => Ok(()),
            other => Err(AgentCommunicationFailed::UnexpectedMessage(other)),
        }
    }

    pub fn sender(&self) -> &AgentSender {
        &self.sender
    }

    pub async fn send(&self, message: ClientMessage) -> Result<()> {
        self.sender.send(message).await?;
        Ok(())
    }

    pub async fn receive(&mut self) -> Option<DaemonMessage> {
        self.receiver.recv().await
    }
}

#[derive(Clone)]
pub struct AgentSender(Sender<ClientMessage>);

impl AgentSender {
    pub async fn send(&self, message: ClientMessage) -> Result<()> {
        self.0.send(message).await?;
        Ok(())
    }
}

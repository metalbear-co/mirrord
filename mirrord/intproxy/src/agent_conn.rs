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

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    ProxyMessage,
};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentConnectInfo {
    /// Connect to the agent through the operator.
    Operator(OperatorSessionInformation),
    /// Connect directly to the agent by name and port using k8s port forward.
    DirectKubernetes(AgentKubernetesConnectInfo),
}

pub struct AgentConnection {
    agent_tx: Sender<ClientMessage>,
    agent_rx: Receiver<DaemonMessage>,
}

impl AgentConnection {
    pub async fn new(
        config: &LayerConfig,
        connect_info: Option<&AgentConnectInfo>,
    ) -> Result<Self, AgentConnectionError> {
        let (agent_tx, agent_rx) = match connect_info.as_ref() {
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

        Ok(AgentConnection { agent_tx, agent_rx })
    }
}

#[derive(Error, Debug)]
#[error("agent unexpectedly closed connection")]
pub struct AgentClosedConnection;

impl BackgroundTask for AgentConnection {
    type Error = AgentClosedConnection;
    type MessageIn = ClientMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => break Ok(()),
                    Some(msg) => self.agent_tx.send(msg).await.map_err(|_| AgentClosedConnection)?,
                },

                msg = self.agent_rx.recv() => match msg {
                    None => break Err(AgentClosedConnection),
                    Some(msg) => message_bus.send(ProxyMessage::FromAgent(msg)).await,
                }
            }
        }
    }
}

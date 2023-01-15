use std::time::Duration;

use mirrord_config::LayerConfig;
use mirrord_kube::api::{kubernetes::KubernetesAPI, AgentManagment};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::mpsc;

use crate::{CliError, Result};

pub(crate) enum AgentConnectInfo {
    /// No info is needed, operator creates on demand
    Operator,
    /// Connect directly to an agent by name and port using k8s port forward.
    DirectKubernetes(String, u16),
}

pub(crate) struct AgentConnection {
    pub sender: mpsc::Sender<ClientMessage>,
    pub receiver: mpsc::Receiver<DaemonMessage>,
}

/// Creates an agent if needed then connects to it.
pub(crate) async fn create_and_connect<P>(
    config: &LayerConfig,
    progress: &P,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
    if config.operator && let Some(connection) = OperatorApi::discover(config).await.transpose() {
        let (sender, receiver) = connection.map_err(CliError::OperatorConnectionFailed)?;

        Ok((
            AgentConnectInfo::Operator,
            AgentConnection { sender, receiver },
        ))
    } else {
        let k8s_api = KubernetesAPI::create(config)
            .await
            .map_err(CliError::KubernetesApiFailed)?;
        let (pod_agent_name, agent_port) = tokio::time::timeout(
            Duration::from_secs(config.agent.startup_timeout),
            k8s_api.create_agent(progress),
        )
        .await
        .map_err(|_| CliError::AgentReadyTimeout)?
        .map_err(CliError::CreateAgentFailed)?;

        let (sender, receiver) = k8s_api
            .create_connection((pod_agent_name.clone(), agent_port))
            .await
            .map_err(CliError::AgentConnectionFailed)?;

        Ok((
            AgentConnectInfo::DirectKubernetes(pod_agent_name, agent_port),
            AgentConnection { sender, receiver },
        ))
    }
}

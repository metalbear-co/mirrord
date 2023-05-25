use std::time::Duration;

use miette::IntoDiagnostic;
use mirrord_config::LayerConfig;
use mirrord_kube::api::{kubernetes::KubernetesAPI, AgentManagment};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::mpsc;
use tracing::trace;

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

pub(crate) async fn connect_operator<P>(
    config: &LayerConfig,
    progress: &P,
) -> Option<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)>
where
    P: Progress + Send + Sync,
{
    let sub_progress = progress.subtask("checking operator");

    match OperatorApi::discover(config, progress)
        .await
        .map_err(CliError::OperatorConnectionFailed)
        .into_diagnostic()
    {
        Ok(Some(connection)) => {
            sub_progress.done_with("connected to operator");

            Some(connection)
        }
        Ok(None) => {
            sub_progress.done_with("no operator detected");

            None
        }
        Err(err) => {
            sub_progress.done_with("unable to check if operator exists, probably due to RBAC");

            trace!("{err}");

            None
        }
    }
}

/// Creates an agent if needed then connects to it.
pub(crate) async fn create_and_connect<P>(
    config: &LayerConfig,
    progress: &P,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
    if config.operator && let Some((sender, receiver)) = connect_operator(config, progress).await {
        Ok((
            AgentConnectInfo::Operator,
            AgentConnection { sender, receiver },
        ))
    } else {
        if matches!(config.target, Some(mirrord_config::target::TargetConfig{ path: mirrord_config::target::Target::Deployment{..}, ..})) {
            // progress.subtask("text").done_with("text");
            eprintln!("When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.\n \
                      Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.\n \
                      To try it out, join the waitlist with `mirrord waitlist <email address>`, or at this link: https://metalbear.co/#waitlist-form");
        }
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

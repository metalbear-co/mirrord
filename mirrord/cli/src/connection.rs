use std::time::Duration;

use mirrord_analytics::AnalyticsReporter;
use mirrord_config::{feature::network::outgoing::OutgoingFilterConfig, LayerConfig};
use mirrord_kube::api::{
    kubernetes::{AgentKubernetesConnectInfo, KubernetesAPI},
    AgentManagment,
};
use mirrord_operator::client::{OperatorApi, OperatorApiError, OperatorSessionInformation};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, trace};

use crate::{CliError, Result};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum AgentConnectInfo {
    Operator(OperatorSessionInformation),
    /// Connect directly to an agent by name and port using k8s port forward.
    DirectKubernetes(AgentKubernetesConnectInfo),
}

const AGENT_CONNECT_INFO_KEY: &str = "MIRRORD_AGENT_CONNECT_INFO";

impl AgentConnectInfo {
    /// Returns environment variable holding the information
    pub const fn env_key() -> &'static str {
        AGENT_CONNECT_INFO_KEY
    }

    /// Loads the information from environment variables
    pub fn from_env() -> Result<Option<Self>> {
        std::env::var(Self::env_key())
            .ok()
            .map(|val| {
                serde_json::from_str(&val).map_err(|e| CliError::ConnectInfoLoadFailed(val, e))
            })
            .transpose()
    }
}
pub(crate) struct AgentConnection {
    pub sender: mpsc::Sender<ClientMessage>,
    pub receiver: mpsc::Receiver<DaemonMessage>,
}

pub(crate) async fn create_operator_session<P>(
    config: &LayerConfig,
    progress: &P,
    analytics: &mut AnalyticsReporter,
) -> Result<
    Option<(
        mpsc::Sender<ClientMessage>,
        mpsc::Receiver<DaemonMessage>,
        OperatorSessionInformation,
    )>,
    CliError,
>
where
    P: Progress + Send + Sync,
{
    let mut sub_progress = progress.subtask("checking operator");

    match OperatorApi::create_session(config, progress, analytics).await {
        Ok(Some(connection)) => {
            sub_progress.success(Some("connected to operator"));
            Ok(Some(connection))
        }
        Ok(None) => {
            sub_progress.success(Some("no operator detected"));

            Ok(None)
        }
        Err(OperatorApiError::ConcurrentStealAbort) => {
            sub_progress.failure(Some("operator concurrent port steal lock"));

            Err(CliError::OperatorConcurrentSteal)
        }
        Err(err) => {
            sub_progress.failure(Some(
                "unable to check if operator exists, probably due to RBAC",
            ));

            trace!(
                "{}",
                miette::Error::from(CliError::OperatorConnectionFailed(err))
            );

            Ok(None)
        }
    }
}

/// Creates an agent if needed then connects to it.
pub(crate) async fn create_and_connect<P>(
    config: &LayerConfig,
    progress: &mut P,
    analytics: &mut AnalyticsReporter,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
    if let Some(outgoing_filter) = &config.feature.network.outgoing.filter {
        if matches!(outgoing_filter, OutgoingFilterConfig::Remote(_)) && !config.feature.network.dns
        {
            progress.warning(
                    "The mirrord outgoing traffic filter includes host names to be connected remotely,\
                     but the remote DNS feature is disabled, so the addresses of these hosts will be\
                     resolved locally!\n\
                     > Consider enabling the remote DNS resolution feature.",
            );
        }
    }

    if config.operator && let Some((sender, receiver, operator_information)) = create_operator_session(config, progress, analytics).await? {
        Ok((
            AgentConnectInfo::Operator(operator_information),
            AgentConnection { sender, receiver },
        ))
    } else {
        if matches!(config.target, mirrord_config::target::TargetConfig{ path: Some(mirrord_config::target::Target::Deployment{..}), ..}) {
            // This is CLI Only because the extensions also implement this check with better messaging.
            progress.print( "When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.");
            progress.print("Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.");
            progress.print("To try it out, join the waitlist with `mirrord waitlist <email address>`, or at this link: https://metalbear.co/#waitlist-form");
        }
        let k8s_api = KubernetesAPI::create(config)
            .await
            .map_err(CliError::KubernetesApiFailed)?;

        let _ = k8s_api.detect_openshift(progress).await.map_err(|err| {
            debug!("couldn't determine OpenShift: {err}");
        });

        let agent_connect_info = tokio::time::timeout(
            Duration::from_secs(config.agent.startup_timeout),
            k8s_api.create_agent(progress, Some(config)),
        )
        .await
        .map_err(|_| CliError::AgentReadyTimeout)?
        .map_err(CliError::CreateAgentFailed)?;

        let (sender, receiver) = k8s_api
            .create_connection(agent_connect_info.clone())
            .await
            .map_err(CliError::AgentConnectionFailed)?;

        Ok((
            AgentConnectInfo::DirectKubernetes(agent_connect_info),
            AgentConnection { sender, receiver },
        ))
    }
}

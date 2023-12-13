use std::time::Duration;

use mirrord_analytics::AnalyticsReporter;
use mirrord_config::{feature::network::outgoing::OutgoingFilterConfig, LayerConfig};
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_kube::api::{kubernetes::KubernetesAPI, AgentManagment};
use mirrord_operator::client::{OperatorApi, OperatorApiError, OperatorOperation};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::mpsc;

use crate::{CliError, Result};

pub(crate) struct AgentConnection {
    pub sender: mpsc::Sender<ClientMessage>,
    pub receiver: mpsc::Receiver<DaemonMessage>,
}

trait OperatorApiErrorExt {
    /// Whether this error should abort the execution, even if the user did not specify whether to
    /// use the operator or not.
    fn should_abort_cli(&self) -> bool;
}

impl OperatorApiErrorExt for OperatorApiError {
    fn should_abort_cli(&self) -> bool {
        match self {
            // Various kube errors can happen due to RBAC if the operator is not installed.
            Self::KubeError {
                operation: OperatorOperation::FindingOperator,
                ..
            } => false,
            // These should either never happen or can happen only if the operator is installed.
            Self::ConcurrentStealAbort
            | Self::ConnectRequestBuildError(..)
            | Self::CreateApiError(..)
            | Self::InvalidTarget { .. }
            | Self::UnsupportedFeature { .. }
            | Self::KubeError { .. } => true,
        }
    }
}

/// Creates an agent if needed then connects to it.
///
/// First it checks if we have an `operator` in the [`config`](LayerConfig), which we do if the
/// user has installed the mirrord-operator in their cluster, even without a valid license. And
/// then we create a session with the operator with [`create_operator_session`].
///
/// If there is no operator, or the license is not good enough for starting an operator session,
/// then we create the mirrord-agent and run mirrord by itself, without the operator.
///
/// Here is where we start interactions with the kubernetes API.
#[tracing::instrument(level = "trace", skip_all)]
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

    if config.operator != Some(false) {
        let mut subtask = progress.subtask("checking operator");

        match OperatorApi::create_session(config, &subtask, analytics).await {
            Ok(session) => {
                subtask.success(Some("connected to the operator"));
                return Ok((
                    AgentConnectInfo::Operator(session.info),
                    AgentConnection {
                        sender: session.tx,
                        receiver: session.rx,
                    },
                ));
            }
            Err(e) if config.operator == Some(true) || e.should_abort_cli() => return Err(e.into()),
            Err(e) => {
                tracing::trace!("{}", CliError::from(e));
                subtask.success(Some("operator not found"));
            }
        }
    }

    if config.feature.copy_target.enabled {
        return Err(CliError::FeatureRequiresOperatorError("copy target".into()));
    }

    if matches!(
        config.target,
        mirrord_config::target::TargetConfig {
            path: Some(mirrord_config::target::Target::Deployment { .. }),
            ..
        }
    ) {
        // This is CLI Only because the extensions also implement this check with better messaging.
        progress.print( "When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.");
        progress.print("Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.");
        progress.print("To try it out, join the waitlist with `mirrord waitlist <email address>`, or at this link: https://metalbear.co/#waitlist-form");
    }

    let k8s_api = KubernetesAPI::create(config)
        .await
        .map_err(CliError::KubernetesApiFailed)?;

    let _ = k8s_api.detect_openshift(progress).await.map_err(|err| {
        tracing::debug!("couldn't determine OpenShift: {err}");
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

pub const AGENT_CONNECT_INFO_ENV_KEY: &str = "MIRRORD_AGENT_CONNECT_INFO";

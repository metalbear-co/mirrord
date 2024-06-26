use std::{collections::HashSet, time::Duration};

use kube::{api::GroupVersionKind, discovery, Resource};
use mirrord_analytics::Reporter;
use mirrord_config::LayerConfig;
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_kube::{
    api::{
        kubernetes::{create_kube_api, KubernetesAPI},
        wrap_raw_connection,
    },
    error::KubeApiError,
};
use mirrord_operator::{
    client::{OperatorApi, OperatorApiError, OperatorOperation},
    crd::MirrordOperatorCrd,
};
use mirrord_progress::{
    messages::MULTIPOD_WARNING, IdeAction, IdeMessage, NotificationLevel, Progress,
};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::mpsc;

use crate::{CliError, Result};

pub(crate) struct AgentConnection {
    pub sender: mpsc::Sender<ClientMessage>,
    pub receiver: mpsc::Receiver<DaemonMessage>,
}

#[tracing::instrument(level = "trace", skip(config), ret, err)]
async fn check_if_operator_resource_exists(config: &LayerConfig) -> Result<bool, KubeApiError> {
    let client = create_kube_api(
        config.accept_invalid_certificates,
        config.kubeconfig.clone(),
        config.kube_context.clone(),
    )
    .await?;

    let gvk = GroupVersionKind {
        group: MirrordOperatorCrd::group(&()).into_owned(),
        version: MirrordOperatorCrd::version(&()).into_owned(),
        kind: MirrordOperatorCrd::kind(&()).into_owned(),
    };

    match discovery::oneshot::pinned_kind(&client, &gvk).await {
        Ok(..) => Ok(true),
        Err(kube::Error::Api(response)) if response.code == 404 => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Creates an agent if needed then connects to it.
///
/// First it checks if we have an `operator` in the [`config`](LayerConfig), which we do if the
/// user has installed the mirrord-operator in their cluster, even without a valid license. And
/// then we create a session with the operator with [`OperatorApi::create_session`].
///
/// If there is no operator, or the license is not good enough for starting an operator session,
/// then we create the mirrord-agent and run mirrord by itself, without the operator.
///
/// Here is where we start interactions with the kubernetes API.
#[tracing::instrument(level = "trace", skip_all)]
pub(crate) async fn create_and_connect<P, R: Reporter>(
    config: &LayerConfig,
    progress: &mut P,
    analytics: &mut R,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
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

            Err(OperatorApiError::NoLicense) if config.operator.is_none() => {
                tracing::trace!("operator license expired");
                subtask.success(Some("operator license expired"));
            }

            Err(
                e @ OperatorApiError::KubeError {
                    operation: OperatorOperation::FindingOperator,
                    ..
                },
            ) if config.operator.is_none() => {
                // We need to check if the operator is really installed or not.
                match check_if_operator_resource_exists(config).await {
                    // Operator is installed yet we failed to use it, abort
                    Ok(true) => {
                        return Err(e.into());
                    }
                    // Operator is not installed, fallback to OSS
                    Ok(false) => {
                        subtask.success(Some("operator not found"));
                    }
                    // We don't know if operator is installed or not,
                    // prompt a warning and fallback to OSS
                    Err(error) => {
                        let message = "failed to check if operator is installed";
                        tracing::debug!(%error, message);
                        subtask.warning(message);
                        subtask.success(Some("operator not found"));
                    }
                }
            }

            Err(e) => return Err(e.into()),
        }
    }

    if config.feature.copy_target.enabled {
        return Err(CliError::FeatureRequiresOperatorError("copy_target".into()));
    }

    if matches!(
        config.target,
        mirrord_config::target::TargetConfig {
            path: Some(
                mirrord_config::target::Target::Deployment { .. }
                    | mirrord_config::target::Target::Rollout(..)
            ),
            ..
        }
    ) {
        // Send to IDEs that we're in multi-pod without operator.
        progress.ide(serde_json::to_value(IdeMessage {
            id: MULTIPOD_WARNING.0.to_string(),
            level: NotificationLevel::Warning,
            text: MULTIPOD_WARNING.1.to_string(),
            actions: {
                let mut actions = HashSet::new();
                actions.insert(IdeAction::Link {
                    label: "Try it now".to_string(),
                    link: "https://app.metalbear.co/".to_string(),
                });

                actions
            },
        })?);
        // This is CLI Only because the extensions also implement this check with better messaging.
        progress.print( "When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.");
        progress.print("Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.");
        progress.print("You can get started with mirrord for Teams at this link: https://mirrord.dev/docs/overview/teams/");
    }

    let k8s_api = KubernetesAPI::create(config)
        .await
        .map_err(CliError::CreateAgentFailed)?;

    if let Err(error) = k8s_api.detect_openshift(progress).await {
        tracing::debug!(?error, "Failed to detect OpenShift");
    };

    let agent_connect_info = tokio::time::timeout(
        Duration::from_secs(config.agent.startup_timeout),
        k8s_api.create_agent(progress, &config.target, Some(config), Default::default()),
    )
    .await
    .unwrap_or(Err(KubeApiError::AgentReadyTimeout))
    .map_err(CliError::CreateAgentFailed)?;

    let (sender, receiver) = wrap_raw_connection(
        k8s_api
            .create_connection(agent_connect_info.clone())
            .await
            .map_err(CliError::AgentConnectionFailed)?,
    );

    Ok((
        AgentConnectInfo::DirectKubernetes(agent_connect_info),
        AgentConnection { sender, receiver },
    ))
}

pub const AGENT_CONNECT_INFO_ENV_KEY: &str = "MIRRORD_AGENT_CONNECT_INFO";

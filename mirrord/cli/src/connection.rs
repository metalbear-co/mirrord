use std::{collections::HashSet, time::Duration};

use mirrord_analytics::Reporter;
use mirrord_config::{target::Target, LayerConfig};
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_kube::{
    api::{kubernetes::KubernetesAPI, wrap_raw_connection},
    error::KubeApiError,
    resolved::ResolvedTarget,
};
use mirrord_operator::client::{OperatorApi, OperatorSessionConnection};
use mirrord_progress::{
    messages::{HTTP_FILTER_WARNING, MULTIPOD_WARNING},
    IdeAction, IdeMessage, NotificationLevel, Progress,
};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::mpsc;
use tracing::Level;

use crate::{CliError, Result};

pub const AGENT_CONNECT_INFO_ENV_KEY: &str = "MIRRORD_AGENT_CONNECT_INFO";

pub(crate) struct AgentConnection {
    pub sender: mpsc::Sender<ClientMessage>,
    pub receiver: mpsc::Receiver<DaemonMessage>,
}

/// 1. If mirrord-operator is explicitly enabled in the given [`LayerConfig`], makes a connection
///    with the target using the mirrord-operator.
/// 2. If mirrord-operator is explicitly disabled in the given [`LayerConfig`], returns [`None`].
/// 3. Otherwise, attempts to use the mirrord-operator and returns [`None`] in case mirrord-operator
///    is not found or its license is invalid.
async fn try_connect_using_operator<P, R>(
    config: &LayerConfig,
    progress: &P,
    analytics: &mut R,
) -> Result<Option<OperatorSessionConnection>>
where
    P: Progress,
    R: Reporter,
{
    let mut operator_subtask = progress.subtask("checking operator");
    if config.operator == Some(false) {
        operator_subtask.success(Some("operator disabled"));
        return Ok(None);
    }

    let api = match OperatorApi::try_new(config, analytics).await? {
        Some(api) => api,
        None if config.operator == Some(true) => return Err(CliError::OperatorNotInstalled),
        None => {
            operator_subtask.success(Some("operator not found"));
            return Ok(None);
        }
    };

    let mut version_cmp_subtask = operator_subtask.subtask("checking version compatibility");
    let compatible = api.check_operator_version(&version_cmp_subtask);
    if compatible {
        version_cmp_subtask.success(Some("operator version compatible"));
    } else {
        version_cmp_subtask.failure(Some("operator version may not be compatible"));
    }

    let mut license_subtask = operator_subtask.subtask("checking license");
    match api.check_license_validity(&license_subtask) {
        Ok(()) => license_subtask.success(Some("operator license valid")),
        Err(error) => {
            license_subtask.failure(Some("operator license expired"));

            if config.operator == Some(true) {
                return Err(error.into());
            } else {
                operator_subtask.failure(Some("proceeding without operator"));
                return Ok(None);
            }
        }
    }

    let mut user_cert_subtask = operator_subtask.subtask("preparing user credentials");
    let api = api.prepare_client_cert(analytics).await.into_certified()?;
    user_cert_subtask.success(Some("user credentials prepared"));

    let target = ResolvedTarget::new(
        api.client(),
        &config.target.path.clone().unwrap_or(Target::Targetless),
        config.target.namespace.as_deref(),
    )
    .await
    .map_err(CliError::OperatorTargetResolution)?;

    let mut session_subtask = operator_subtask.subtask("starting session");
    let connection = api
        .connect_in_new_session(target, config, &session_subtask)
        .await?;
    session_subtask.success(Some("session started"));

    operator_subtask.success(Some("using operator"));

    Ok(Some(connection))
}

/// 1. If mirrord-operator is explicitly enabled in the given [`LayerConfig`], makes a connection
///    with the target using the mirrord-operator.
/// 2. If mirrord-operator is explicitly disabled in the given [`LayerConfig`], creates a
///    mirrord-agent and runs session without the mirrord-operator.
/// 3. Otherwise, attempts to use the mirrord-operator and falls back to OSS flow in case
///    mirrord-operator is not found or its license is invalid.
///
/// Here is where we start interactions with the kubernetes API.
#[tracing::instrument(level = Level::TRACE, skip_all)]
pub(crate) async fn create_and_connect<P, R: Reporter>(
    config: &LayerConfig,
    progress: &mut P,
    analytics: &mut R,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
    if let Some(connection) = try_connect_using_operator(config, progress, analytics).await? {
        return Ok((
            AgentConnectInfo::Operator(connection.session),
            AgentConnection {
                sender: connection.tx,
                receiver: connection.rx,
            },
        ));
    }

    if config.feature.copy_target.enabled {
        return Err(CliError::FeatureRequiresOperatorError("copy_target".into()));
    }

    match (
        // user in mutipod without operator
        matches!(
            config.target,
            mirrord_config::target::TargetConfig {
                path: Some(
                    mirrord_config::target::Target::Deployment { .. }
                        | mirrord_config::target::Target::Rollout(..)
                ),
                ..
            }
        ),
        // user using http filter(s) without operator
        config.feature.network.incoming.http_filter.is_filter_set(),
    ) {
        (true, true) => {
            // only show user one of the two msgs - each user should always be shown same msg
            if user_persistent_random_message_select() {
                show_multipod_warning(progress)?
            } else {
                show_http_filter_warning(progress)?
            }
        }
        (true, false) => show_multipod_warning(progress)?,
        (false, true) => show_http_filter_warning(progress)?,
        _ => (),
    };

    let k8s_api = KubernetesAPI::create(config)
        .await
        .map_err(|error| CliError::auth_exec_error_or(error, CliError::CreateAgentFailed))?;

    if let Err(fail) = k8s_api.detect_openshift(progress).await {
        tracing::debug!(?fail, "Failed to detect OpenShift");

        if let KubeApiError::InvalidCertificate(fail) = fail {
            tracing::warn!(fail);
        }
    };

    let agent_connect_info = tokio::time::timeout(
        Duration::from_secs(config.agent.startup_timeout),
        k8s_api.create_agent(progress, &config.target, Some(config), Default::default()),
    )
    .await
    .unwrap_or(Err(KubeApiError::AgentReadyTimeout))
    .map_err(|error| CliError::auth_exec_error_or(error, CliError::CreateAgentFailed))?;

    let (sender, receiver) = wrap_raw_connection(
        k8s_api
            .create_connection(agent_connect_info.clone())
            .await
            .map_err(|error| {
                CliError::auth_exec_error_or(error, CliError::AgentConnectionFailed)
            })?,
    );

    Ok((
        AgentConnectInfo::DirectKubernetes(agent_connect_info),
        AgentConnection { sender, receiver },
    ))
}

fn user_persistent_random_message_select() -> bool {
    mid::get("mirrord")
        .inspect_err(|error| tracing::error!(%error, "failed to obtain machine ID"))
        .ok()
        .unwrap_or_default()
        .as_bytes()
        .iter()
        .copied()
        .reduce(u8::wrapping_add)
        .unwrap_or_default()
        % 2
        == 0
}

pub(crate) fn show_multipod_warning<P>(progress: &mut P) -> Result<(), CliError>
where
    P: Progress + Send + Sync,
{
    // Send to IDEs that we're in multi-pod without operator.
    progress.ide(serde_json::to_value(IdeMessage {
            id: MULTIPOD_WARNING.0.to_string(),
            level: NotificationLevel::Warning,
            text: MULTIPOD_WARNING.1.to_string(),
            actions: {
                let mut actions = HashSet::new();
                actions.insert(IdeAction::Link {
                    label: "Get started (read the docs)".to_string(),
                    link: "https://mirrord.dev/docs/overview/teams/?utm_source=multipodwarn&utm_medium=plugin".to_string(),
                });
                actions.insert(IdeAction::Link {
                    label: "Try it now".to_string(),
                    link: "https://app.metalbear.co/".to_string(),
                });

                actions
            },
        })?);
    // This is CLI Only because the extensions also implement this check with better messaging.
    progress.print("When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.");
    progress.print("Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.");
    progress.print("You can get started with mirrord for Teams at this link: https://mirrord.dev/docs/overview/teams/?utm_source=multipodwarn&utm_medium=cli");
    Ok(())
}

pub(crate) fn show_http_filter_warning<P>(progress: &mut P) -> Result<(), CliError>
where
    P: Progress + Send + Sync,
{
    // Send to IDEs that at an HTTP filter is set without operator.
    progress.ide(serde_json::to_value(IdeMessage {
        id: HTTP_FILTER_WARNING.0.to_string(),
        level: NotificationLevel::Warning,
        text: HTTP_FILTER_WARNING.1.to_string(),
        actions: {
            let mut actions = HashSet::new();
            actions.insert(IdeAction::Link {
                label: "Get started (read the docs)".to_string(),
                link: "https://mirrord.dev/docs/overview/teams/?utm_source=httpfilter&utm_medium=plugin".to_string(),
            });
            actions.insert(IdeAction::Link {
                label: "Try it now".to_string(),
                link: "https://app.metalbear.co/".to_string(),
            });

            actions
        },
    })?);
    // This is CLI Only because the extensions also implement this check with better messaging.
    progress.print("You're using an HTTP filter, which generally indicates the use of a shared environment. If so, we recommend");
    progress.print("considering mirrord for Teams, which is better suited to shared environments.");
    progress.print("You can get started with mirrord for Teams at this link: https://mirrord.dev/docs/overview/teams/?utm_source=httpfilter&utm_medium=cli");
    Ok(())
}

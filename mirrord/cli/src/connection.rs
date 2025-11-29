use std::{collections::HashSet, time::Duration};

use mirrord_analytics::Reporter;
use mirrord_config::{
    LayerConfig,
    target::{Target, TargetDisplay},
};
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_kube::{
    api::{container::ContainerConfig, kubernetes::KubernetesAPI},
    error::KubeApiError,
    resolved::ResolvedTarget,
};
use mirrord_operator::client::{OperatorApi, OperatorSessionConnection};
use mirrord_progress::{
    IdeAction, IdeMessage, NotificationLevel, Progress,
    messages::{HTTP_FILTER_WARNING, MULTIPOD_WARNING},
};
use mirrord_protocol_io::{Client, Connection};
use tracing::Level;

use crate::{CliError, CliResult, MirrordCi, ci::error::CiError};

pub const AGENT_CONNECT_INFO_ENV_KEY: &str = "MIRRORD_AGENT_CONNECT_INFO";

/// 1. If mirrord-operator is explicitly enabled in the given [`LayerConfig`], makes a connection
///    with the target using the mirrord-operator.
/// 2. If mirrord-operator is explicitly disabled in the given [`LayerConfig`], returns [`None`].
/// 3. Otherwise, attempts to use the mirrord-operator and returns [`None`] in case mirrord-operator
///    is not found or its license is invalid.
async fn try_connect_using_operator<P, R>(
    layer_config: &mut LayerConfig,
    progress: &P,
    analytics: &mut R,
    branch_name: Option<String>,
    mirrord_for_ci: Option<&MirrordCi>,
) -> CliResult<Option<OperatorSessionConnection>>
where
    P: Progress,
    R: Reporter,
{
    let mut operator_subtask = progress.subtask("checking operator");
    if layer_config.operator == Some(false) {
        operator_subtask.success(Some("operator disabled"));
        return Ok(None);
    }

    let api = match OperatorApi::try_new(layer_config, analytics, progress).await? {
        Some(api) => api,
        None if layer_config.operator == Some(true) => {
            return Err(CliError::OperatorNotInstalled);
        }
        None => {
            operator_subtask.success(Some("operator not found"));
            return Ok(None);
        }
    };

    let mut license_subtask = operator_subtask.subtask("checking license");
    match api.check_license_validity(&license_subtask) {
        Ok(()) => license_subtask.success(Some("operator license valid")),
        Err(error) => {
            license_subtask.failure(Some("operator license expired"));

            if layer_config.operator == Some(true) {
                return Err(error.into());
            } else {
                operator_subtask.failure(Some("proceeding without operator"));
                return Ok(None);
            }
        }
    }

    let mut user_cert_subtask = operator_subtask.subtask("preparing user credentials");
    let api = match mirrord_for_ci {
        Some(mirrord_for_ci) => {
            api.with_ci_api_key(
                analytics,
                progress,
                layer_config,
                mirrord_for_ci.api_key().ok_or(CiError::MissingCiApiKey)?,
            )
            .await
        }
        None => {
            api.with_client_certificate(analytics, progress, layer_config)
                .await
        }
    }
    .into_certified()?;

    user_cert_subtask.success(Some("user credentials prepared"));

    let target = ResolvedTarget::new(
        api.client(),
        &layer_config
            .target
            .path
            .clone()
            .unwrap_or(Target::Targetless),
        layer_config.target.namespace.as_deref(),
    )
    .await
    .map_err(CliError::OperatorTargetResolution)?;

    let mut session_subtask = operator_subtask.subtask("starting session");
    let connection = api
        .connect_in_new_session(target, layer_config, &session_subtask, branch_name)
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
#[tracing::instrument(level = Level::TRACE, skip_all, err)]
pub(crate) async fn create_and_connect<P: Progress, R: Reporter>(
    config: &mut LayerConfig,
    progress: &mut P,
    analytics: &mut R,
    branch_name: Option<String>,
    mirrord_for_ci: Option<&MirrordCi>,
) -> CliResult<(AgentConnectInfo, Connection<Client>)> {
    if let Some(connection) =
        try_connect_using_operator(config, progress, analytics, branch_name, mirrord_for_ci).await?
    {
        return Ok((
            AgentConnectInfo::Operator(connection.session),
            connection.conn,
        ));
    }

    process_config_oss(config, progress)?;

    let k8s_api = KubernetesAPI::create(config, progress)
        .await
        .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateAgentFailed))?;

    k8s_api
        .detect_openshift(progress)
        .await
        .map_err(|fail| CliError::friendlier_error_or_else(fail, CliError::CreateAgentFailed))
        .inspect_err(|fail| tracing::debug!(?fail, "Failed to detect OpenShift!"))
        .ok();

    let agent_container_config = ContainerConfig {
        support_ipv6: config.feature.network.ipv6,
        ..Default::default()
    };
    let agent_connect_info = tokio::time::timeout(
        Duration::from_secs(config.agent.startup_timeout),
        k8s_api.create_agent(
            progress,
            &config.target,
            Some(&mut config.feature.network),
            agent_container_config,
        ),
    )
    .await
    .unwrap_or(Err(KubeApiError::AgentReadyTimeout))
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateAgentFailed))?;

    let conn = Connection::<Client>::from_stream(
        k8s_api
            .create_connection_portforward(agent_connect_info.clone())
            .await
            .map_err(|error| {
                CliError::friendlier_error_or_else(error, CliError::AgentConnectionFailed)
            })?,
    )
    .await?;

    Ok((AgentConnectInfo::DirectKubernetes(agent_connect_info), conn))
}

/// Verifies and adjusts the [`LayerConfig`] after we've determined that this run does not use the
/// operator.
fn process_config_oss<P: Progress>(config: &mut LayerConfig, progress: &mut P) -> CliResult<()> {
    // operator is disabled, but target requires it.
    if let Some(target) = config.target.path.as_ref()
        && Target::requires_operator(target)
    {
        return Err(CliError::FeatureRequiresOperatorError(format!(
            "target type {}",
            target.type_()
        )));
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

    Ok(())
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

pub(crate) fn show_multipod_warning<P>(progress: &mut P) -> CliResult<(), CliError>
where
    P: Progress,
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
                    link: "https://metalbear.com/mirrord/docs/overview/teams/?utm_source=multipodwarn&utm_medium=plugin".to_string(),
                });
                actions.insert(IdeAction::Link {
                    label: "Try it now".to_string(),
                    link: "https://app.metalbear.com/".to_string(),
                });

                actions
            },
        })?);
    // This is CLI Only because the extensions also implement this check with better messaging.
    progress.print("When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.");
    progress.print("Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.");
    progress.print("You can get started with mirrord for Teams at this link: https://metalbear.com/mirrord/docs/overview/teams/?utm_source=multipodwarn&utm_medium=cli");
    Ok(())
}

pub(crate) fn show_http_filter_warning<P>(progress: &mut P) -> CliResult<(), CliError>
where
    P: Progress,
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
                link: "https://metalbear.com/mirrord/docs/overview/teams/?utm_source=httpfilter&utm_medium=plugin".to_string(),
            });
            actions.insert(IdeAction::Link {
                label: "Try it now".to_string(),
                link: "https://app.metalbear.com/".to_string(),
            });

            actions
        },
    })?);
    // This is CLI Only because the extensions also implement this check with better messaging.
    progress.print("You're using an HTTP filter, which generally indicates the use of a shared environment. If so, we recommend");
    progress.print("considering mirrord for Teams, which is better suited to shared environments.");
    progress.print("You can get started with mirrord for Teams at this link: https://metalbear.com/mirrord/docs/overview/teams/?utm_source=httpfilter&utm_medium=cli");
    Ok(())
}

#[cfg(test)]
mod tests {
    use mirrord_config::{
        LayerFileConfig,
        config::{ConfigContext, MirrordConfig},
        target::{Target, TargetFileConfig, pod::PodTarget, service::ServiceTarget},
    };
    use mirrord_progress::NullProgress;
    use rstest::rstest;

    use crate::connection::process_config_oss;

    /// Ensure that when `process_config_oss` is called, operator-only target types are disallowed.
    /// This occurs when `create_and_connect` fails to establish a connection with the operator.
    #[rstest]
    #[case(Target::Pod(PodTarget{pod: "my-pet-pod".into(),container: None}))]
    #[case(Target::Service(
        ServiceTarget{service: "service-for-world-domination".into(),container: None}
    ))]
    fn deny_non_oss_targets_without_operator(#[case] target: Target) {
        let allowed = !target.requires_operator();

        let mut cfg_context = ConfigContext::default().strict_env(true);
        let mut config = LayerFileConfig {
            target: Some(TargetFileConfig::Simple(Some(target))),
            ..Default::default()
        }
        .generate_config(&mut cfg_context)
        .unwrap();
        let mut progress = NullProgress {};

        assert_eq!(
            process_config_oss(&mut config, &mut progress).is_ok(),
            allowed
        )
    }
}

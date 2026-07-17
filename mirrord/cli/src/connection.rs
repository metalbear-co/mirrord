use std::{collections::HashSet, ops::Not, time::Duration};

use mirrord_analytics::Reporter;
use mirrord_config::{
    LayerConfig,
    agent::AgentFileConfig,
    config::MirrordConfig,
    target::{Target, TargetDisplay},
};
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_kube::{
    api::{
        container::ContainerConfig,
        kubernetes::{KubernetesAPI, apiserver_version},
    },
    error::KubeApiError,
    resolved::ResolvedTarget,
};
use mirrord_operator::{
    client::{OperatorApi, OperatorSessionConnection},
    crd::NewOperatorFeature,
};
use mirrord_progress::{
    IdeAction, IdeMessage, NotificationLevel, Progress,
    messages::{HTTP_FILTER_WARNING, MULTIPOD_WARNING},
    utm_medium,
};
use mirrord_protocol_io::{Client, Connection};
use tracing::Level;

use crate::{CliError, CliResult, MirrordCi, ci::error::CiError, up::MirrordUp};

pub const AGENT_CONNECT_INFO_ENV_KEY: &str = "MIRRORD_AGENT_CONNECT_INFO";

/// Sends a "Sign up for mirrord for Teams" upgrade CTA to the IDE via the [`IdeMessage`] protocol.
///
/// Only has an effect when running inside an IDE (`MIRRORD_PROGRESS_MODE=json`).
/// In CLI mode, [`Progress::ide`] is a no-op.
fn send_upgrade_ide_message<P: Progress>(
    progress: &P,
    text: &str,
    utm_source: &str,
) -> CliResult<()> {
    let mut actions = HashSet::new();
    actions.insert(IdeAction::Link {
        label: "Sign up for mirrord for Teams".to_owned(),
        link: format!(
            "https://app.metalbear.com/?utm_source={utm_source}&utm_medium={}",
            utm_medium()
        ),
    });
    progress.ide(serde_json::to_value(IdeMessage {
        id: "upgrade_cta".to_owned(),
        level: NotificationLevel::Warning,
        text: text.to_owned(),
        actions,
    })?);
    Ok(())
}

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
    mirrord_up: Option<&MirrordUp>,
) -> CliResult<Option<(OperatorSessionConnection, (u16, u16))>>
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
            send_upgrade_ide_message(
                progress,
                "mirrord operator was not found in the cluster.",
                "operatornotinstalled",
            )?;
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
                send_upgrade_ide_message(
                    progress,
                    "mirrord operator license expired.",
                    "licenseexpired",
                )?;
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

    let is_multi_cluster = api
        .operator()
        .spec
        .supported_features()
        .contains(&NewOperatorFeature::MultiClusterPrimary)
        && layer_config.multi_cluster != Some(false)
        && matches!(layer_config.target.path, Some(Target::Label(_))).not();

    // What happens to unmatched requests on a filtered copy target depends on the operator, so the
    // warning can only be decided once we know which operator we're connected to. An operator with
    // `CopyTargetFilterIsolation` isolates the copy and steals from the original pods, so unmatched
    // requests are served normally - the only exception is a scaled-down target, where there are no
    // originals and the filter is ignored. Older operators steal from the copy itself, discarding
    // every unmatched request.
    if layer_config.feature.copy_target.enabled
        && layer_config
            .feature
            .network
            .incoming
            .http_filter
            .is_filter_set()
    {
        let isolates_copy = api
            .operator()
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::CopyTargetFilterIsolation);

        if isolates_copy {
            if layer_config.feature.copy_target.scale_down {
                progress.warning(
                    "copy target is scaled down and an HTTP filter is set: with the original \
                    workload scaled to zero there are no pods to serve unmatched requests, so the \
                    filter is ignored and all traffic is stolen to your local process",
                );
            }
        } else {
            progress.warning(
                "copy target is enabled and an HTTP filter is set, this means that all unmatched \
                HTTP requests are discarded",
            );
        }
    }

    let mut session_subtask = operator_subtask.subtask("starting session");
    let up_session_info = mirrord_up.map(MirrordUp::info);
    let connection = if is_multi_cluster {
        // Multi-cluster: CLI connects to Primary, which routes to the workload cluster
        // where the target is resolved and the session is created
        let target_config = layer_config
            .target
            .path
            .clone()
            .unwrap_or(Target::Targetless);

        if layer_config.feature.magic.auto_mount {
            progress.warning(
                "auto_mount: skipped in multi-cluster mode (target is resolved server-side)",
            );
        }

        api.connect_in_multi_cluster_session(
            &target_config,
            layer_config,
            &session_subtask,
            branch_name,
            mirrord_for_ci.map(MirrordCi::info),
            up_session_info.clone(),
        )
        .await?
    } else {
        // Single-cluster: CLI resolves target of the connected cluster
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

        if layer_config.feature.magic.auto_mount {
            apply_auto_mount_for_target(layer_config, &target, api.client(), progress).await;
        }

        api.connect_in_new_session(
            target,
            layer_config,
            &session_subtask,
            branch_name,
            mirrord_for_ci.map(MirrordCi::info),
            up_session_info,
        )
        .await?
    };
    session_subtask.success(Some("session started"));

    operator_subtask.success(Some("using operator"));

    let api_version = apiserver_version(api.client())
        .await
        .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateAgentFailed))?;

    Ok(Some((connection, api_version)))
}

pub(crate) struct ConnectData {
    pub(crate) info: AgentConnectInfo,
    pub(crate) connection: Connection<Client>,
    /// Kube apiserver version (major, minor).
    pub(crate) api_version: (u16, u16),
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
    mirrord_up: Option<&MirrordUp>,
) -> CliResult<ConnectData> {
    if let Some((connection, api_version)) = try_connect_using_operator(
        config,
        progress,
        analytics,
        branch_name,
        mirrord_for_ci,
        mirrord_up,
    )
    .await?
    {
        if config.agent
            != AgentFileConfig::default()
                .generate_config(&mut Default::default())
                .expect("BUG: Default agent config should always work!")
        {
            progress.warning(
                "Agent configuration is ignored when using the mirrord operator. \
                 Agent configuration is managed by the cluster admin.",
            );
        }
        return Ok(ConnectData {
            info: AgentConnectInfo::Operator(connection.session),
            connection: Connection::from_channel(connection.conn),
            api_version,
        });
    }

    process_config_oss(config, progress)?;

    let k8s_api = KubernetesAPI::create(config, progress)
        .await
        .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateAgentFailed))?;

    let api_version = apiserver_version(k8s_api.client())
        .await
        .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateAgentFailed))?;

    k8s_api
        .detect_openshift(progress)
        .await
        .map_err(|fail| CliError::friendlier_error_or_else(fail, CliError::CreateAgentFailed))
        .inspect_err(|fail| tracing::debug!(?fail, "Failed to detect OpenShift!"))
        .ok();

    let auto_mount_target = config
        .feature
        .magic
        .auto_mount
        .then(|| config.target.path.clone())
        .flatten()
        .filter(|target| matches!(target, Target::Targetless).not());

    if let Some(target_path) = auto_mount_target {
        match ResolvedTarget::new(
            k8s_api.client(),
            &target_path,
            config.target.namespace.as_deref(),
        )
        .await
        {
            Ok(target) => {
                apply_auto_mount_for_target(config, &target, k8s_api.client(), progress).await
            }
            Err(error) => {
                tracing::debug!(%error, "auto_mount: failed to resolve target, skipping")
            }
        }
    }

    let agent_container_config = ContainerConfig::default();

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
    );

    Ok(ConnectData {
        info: AgentConnectInfo::DirectKubernetes(agent_connect_info),
        connection: conn,
        api_version,
    })
}

/// Resolves `target`'s pod spec and applies `feature.magic.auto_mount` to `config`, adding the
/// target container's volume-mount paths to `feature.fs.read_only`.
///
/// auto_mount is a best-effort convenience, so any failure to resolve the pod spec is logged and
/// ignored rather than aborting the session.
async fn apply_auto_mount_for_target<P: Progress>(
    config: &mut LayerConfig,
    target: &ResolvedTarget<false>,
    client: &kube::Client,
    progress: &P,
) {
    match target.resolve_pod_spec(client).await {
        Ok(Some(pod_spec)) => config.apply_auto_mount(pod_spec.as_ref(), target.container()),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(?error, "failed to resolve target pod spec");
            progress.warning("auto_mount: failed to resolve target pod spec, skipping")
        }
    }
}

/// Verifies and adjusts the [`LayerConfig`] after we've determined that this run does not use the
/// operator.
fn process_config_oss<P: Progress>(config: &mut LayerConfig, progress: &mut P) -> CliResult<()> {
    // operator is disabled, but target requires it.
    if let Some(target) = config.target.path.as_ref()
        && Target::requires_operator(target)
    {
        send_upgrade_ide_message(
            progress,
            &format!(
                "Target type {} requires the mirrord operator, which is part of mirrord for Teams.",
                target.type_()
            ),
            "requiresoperator",
        )?;
        return Err(CliError::FeatureRequiresOperatorError(format!(
            "target type {}",
            target.type_()
        )));
    }

    if config.feature.copy_target.enabled {
        send_upgrade_ide_message(
            progress,
            "copy_target requires the mirrord operator, which is part of mirrord for Teams.",
            "requiresoperator",
        )?;
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

    config.experimental.disable_reuseaddr = config.experimental.disable_reuseaddr.or(Some(true));
    config.experimental.go_asmcgocall = config.experimental.go_asmcgocall.or(Some(true));

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
        id: MULTIPOD_WARNING.0.to_owned(),
        level: NotificationLevel::Warning,
        text: MULTIPOD_WARNING.1.to_owned(),
        actions: {
            let mut actions = HashSet::new();
            actions.insert(IdeAction::Link {
                label: "Try mirrord for Teams".to_owned(),
                link: "https://app.metalbear.com/?utm_source=multipodwarn&utm_medium=plugin"
                    .to_owned(),
            });

            actions
        },
    })?);
    // This is CLI Only because the extensions also implement this check with better messaging.
    progress.print("When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.");
    progress.print("Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.");
    progress.print("You can get started with mirrord for Teams at this link: https://app.metalbear.com/?utm_source=multipodwarn&utm_medium=cli");
    Ok(())
}

pub(crate) fn show_http_filter_warning<P>(progress: &mut P) -> CliResult<(), CliError>
where
    P: Progress,
{
    // Send to IDEs that at an HTTP filter is set without operator.
    progress.ide(serde_json::to_value(IdeMessage {
        id: HTTP_FILTER_WARNING.0.to_owned(),
        level: NotificationLevel::Warning,
        text: HTTP_FILTER_WARNING.1.to_owned(),
        actions: {
            let mut actions = HashSet::new();
            actions.insert(IdeAction::Link {
                label: "Try mirrord for Teams".to_owned(),
                link: "https://app.metalbear.com/?utm_source=httpfilter&utm_medium=plugin"
                    .to_owned(),
            });

            actions
        },
    })?);
    // This is CLI Only because the extensions also implement this check with better messaging.
    progress.print("You're using an HTTP filter, which generally indicates the use of a shared environment. If so, we recommend");
    progress.print("considering mirrord for Teams, which is better suited to shared environments.");
    progress.print("You can get started with mirrord for Teams at this link: https://app.metalbear.com/?utm_source=httpfilter&utm_medium=cli");
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

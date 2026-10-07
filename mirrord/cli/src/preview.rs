//! Handlers for `mirrord preview` commands.
//!
//! The CLI is responsible for creating `PreviewSession` resources and watching their status —
//! all actual work (pod creation, agent spawning, traffic routing) is done by the operator.
//! The `start` command creates a CR and watches for the status to reach `Ready`, the `status`
//! command lists existing sessions, and the `stop` command deletes them.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    ffi::OsStr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use base64::prelude::*;
use futures::StreamExt;
use glob::Pattern;
use itertools::Itertools;
use k8s_openapi::{ByteString, jiff::Timestamp};
use kube::{
    Api, Client, Resource, ResourceExt,
    api::{DeleteParams, ListParams, ObjectMeta, PostParams},
    runtime::{
        wait::delete,
        watcher::{self, Event, watcher},
    },
};
use mirrord_analytics::{
    AnalyticsError, AnalyticsReporter, ExecutionKind, OperatorWall, Reporter,
    preview::{PreviewEvent, PreviewEventKind},
};
use mirrord_config::{
    LayerConfig,
    config::{ConfigContext, ConfigError, EnvKey},
    feature::{
        network::incoming::tls_delivery::{
            LocalTlsDelivery, TlsClientCertSource, TlsDeliveryProtocol,
        },
        preview::{ConfigMount, ConfigMountType},
    },
    target::{Target, TargetDisplay, label::LabelTarget},
};
use mirrord_kube::api::runtime::RuntimeDataProvider;
use mirrord_operator::{
    client::{NoClientCert, OperatorApi, connect_params::BranchDbNames},
    crd::{
        MirrordOperatorSpec, NewOperatorFeature, TARGET_NAMESPACE_ANNOTATION, TargetCrd,
        preview::{
            PreviewCronJobConfig, PreviewDbBranchingConfig, PreviewEnvVarsConfig,
            PreviewIdleConfig, PreviewIncomingConfig, PreviewLabelFilter, PreviewPodLogs,
            PreviewQueueSplittingConfig, PreviewSecretMountFile, PreviewSession,
            PreviewSessionPhase, PreviewSessionSpec, PreviewTlsClientAuth,
            PreviewTlsClientAuthFromTarget, PreviewTlsDelivery,
            view::{PreviewEnv, PreviewMessageKind},
        },
        session::{KubeResourceTarget, PodSetTarget, SessionTarget},
    },
    types::OPERATOR_OWNERSHIP_LABEL,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Table, row};
use tracing::Level;

use crate::{
    config::{
        PreviewArgs, PreviewCommand, PreviewCommonArgs, PreviewDiffArgs, PreviewLogsArgs,
        PreviewStartArgs, PreviewStatusArgs, PreviewStopArgs,
    },
    data::UserData,
    error::{CliError, CliResult, format_preview_logs},
};

mod multicluster;
pub(crate) mod resources;

/// Handle commands related to preview environments: `mirrord preview ...`
pub(crate) async fn preview_command(
    args: PreviewArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let PreviewArgs { common, command } = args;

    match command {
        PreviewCommand::Start(start_args) => {
            preview_start(&common, start_args, watch, user_data).await
        }
        PreviewCommand::Status(status_args) => {
            preview_status(&common, status_args, watch, user_data).await
        }
        PreviewCommand::Stop(stop_args) => preview_stop(&common, stop_args, watch, user_data).await,
        PreviewCommand::Logs(logs_args) => preview_logs(&common, logs_args, watch, user_data).await,
        PreviewCommand::Diff(diff_args) => preview_diff(&common, diff_args, watch, user_data).await,
    }
}

/// Label key used to store the preview session's label-safe environment key on the CR.
///
/// New sessions store [`EnvKey::to_hashed_label_value`] here. Sessions created by older CLIs
/// stored the raw key, so lookups use a set selector that matches either representation when the
/// raw key itself is a valid Kubernetes label value.
pub const PREVIEW_SESSION_KEY_LABEL: &str = "preview.mirrord.metalbear.co/key";

/// Handle `mirrord preview start` command.
///
/// Creates a new preview environment or updates an existing one by creating
/// a `PreviewSession` resource that the operator will reconcile, then watches
/// the status until `Ready` or failure.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_start(
    common: &PreviewCommonArgs,
    args: PreviewStartArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview start");

    let mut layer_config = load_preview_config(args.as_env_vars(common), &mut progress).await?;

    // Read before contacting the cluster, so a broken manifest fails without side effects.
    let resource_paths = resource_paths(args.resources, &mut layer_config);
    let mut supplied_objects = if resource_paths.is_empty() {
        None
    } else {
        Some(load_manifests(&resource_paths, &progress)?)
    };

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch,
        user_data.machine_id(),
        Some(layer_config.key.as_str().to_owned()),
    );

    let (operator_api, api) =
        create_preview_api(&layer_config, false, &progress, &mut analytics).await?;
    operator_api.check_feature_support(&layer_config, false)?;

    if supplied_objects.is_some() {
        require_spec_resources_support(&operator_api.operator().spec)?;
    }

    let is_cronjob_target = matches!(layer_config.target.path, Some(Target::CronJob(_)));
    if is_cronjob_target {
        operator_api
            .operator()
            .spec
            .require_feature(NewOperatorFeature::PreviewCronJobTarget)?;
    }
    if let Some(Target::Label(label_target)) = &layer_config.target.path {
        operator_api
            .operator()
            .spec
            .require_feature(NewOperatorFeature::PreviewLabelTarget)?;

        // Branch preparation reads a single workload out of the target and a label selector
        // names none, so it would fail later. Refuse here, before the existing session with the
        // same key is replaced, so a bad config never tears down a running preview.
        if !layer_config.feature.db_branches.is_empty() {
            return Err(CliError::UnsupportedTargetConfig(format!(
                "database branching does not support label target `{label_target}`; remove \
                 `feature.db_branches` or target a single workload"
            )));
        }
    }

    // Create the `PreviewSession` resource in the cluster. The CR name is derived from
    // the target with a short random suffix to avoid collisions (e.g. `deploy-my-app-a1b2c3d4`).
    // The operator watches for these resources and reconciles them into preview pods.

    let mut subtask = progress.subtask("creating preview session resource");

    let config_target = layer_config.target.path.as_ref().ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewTargetRequired
    })?;

    let image = layer_config.feature.preview.image.as_ref().ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewImageRequired
    })?;

    // Reject unsupported or invalid delivery before replacing a running preview.
    let mut secret_values = BTreeMap::new();
    // A CronJob preview has no long-running pod to steal traffic to, so incoming is never
    // sent (the config check already warned when the user configured it). The `cronjob`
    // block travels only for cronjob targets, so the CR stays identical to what older CLIs
    // send for every other kind.
    let (incoming, cronjob) = if is_cronjob_target {
        (
            None,
            Some(PreviewCronJobConfig {
                schedule: layer_config.feature.preview.cronjob.schedule.clone(),
                // Only the opt-out travels: the CR stays identical to what older CLIs send
                // for the default, and `None` means "trigger" on the operator side.
                trigger_on_start: (!layer_config.feature.preview.cronjob.trigger_on_start)
                    .then_some(false),
            }),
        )
    } else {
        let mut incoming = PreviewIncomingConfig::from_config(
            &layer_config.feature.network.incoming,
            layer_config.key.as_str(),
        );

        if let Some(incoming) = incoming.as_mut() {
            let tls_config = layer_config
                .feature
                .network
                .incoming
                .tls_delivery
                .as_ref()
                .or(layer_config
                    .feature
                    .network
                    .incoming
                    .https_delivery
                    .as_ref());
            let (tls_delivery, warnings) = resolve_tls_delivery(
                tls_config,
                &mut secret_values,
                &operator_api.operator().spec,
            )?;
            for warning in warnings {
                subtask.warning(&warning);
            }
            incoming.tls_delivery = tls_delivery;
        }

        (incoming, None)
    };

    let session_target = resolve_config_target(
        config_target,
        operator_api.client(),
        layer_config.target.namespace.as_deref(),
    )
    .await
    .inspect_err(|_| subtask.failure(None))?;

    // Planned before an existing session is replaced. `plan` dry-runs the manifests and rejects
    // a template that would widen the preview's access, so either failure leaves the running
    // preview alone.
    if let Some(objects) = supplied_objects.as_mut() {
        resources::resolve_workload_refs(
            operator_api.client(),
            objects,
            resources_target(&session_target).inspect_err(|_| subtask.failure(None))?,
            target_namespace(&operator_api, &layer_config),
        )
        .await
        .inspect_err(|_| subtask.failure(None))?;
    }
    let resource_plan = match &supplied_objects {
        Some(objects) => {
            let plan = resources::plan(
                operator_api.client(),
                objects,
                resources::sources_label(&resource_paths),
                resources_target(&session_target)?,
                target_namespace(&operator_api, &layer_config),
            )
            .await
            .inspect_err(|_| subtask.failure(None))?;
            for message in resources::summary(&plan) {
                subtask.info(&message);
            }
            for note in &plan.notes {
                subtask.warning(note);
            }
            Some(plan)
        }
        None => None,
    };
    let spec_resources = resource_plan
        .as_ref()
        .map(|plan| plan.spec_resources(&mut secret_values))
        .transpose()?
        .flatten();

    // Check for an existing session with the same key+target.
    let key = layer_config.key.as_str();
    let existing_sessions = KeyMatcher::Simple(key)
        .list_matching_sessions(&api)
        .await
        .inspect_err(|_| subtask.failure(None))?;

    for session in existing_sessions
        .into_iter()
        .filter(|session| session.spec.target == session_target)
    {
        let name = session.name_any();

        subtask.warning(&format!("replacing existing session '{name}'"));
        if &session.spec.image == image {
            subtask.warning(&format!("configured image and existing session's image are the same ('{image}'), this command will only restart the existing deployment"));
        }

        // Delete and wait for the existing session to be fully removed.
        match tokio::time::timeout(
            Duration::from_secs(60),
            delete::delete_and_finalize(api.clone(), &name, &DeleteParams::default()),
        )
        .await
        {
            Err(_) => {
                subtask.failure(None);
                return Err(CliError::PreviewDeleteFailed {
                    name: name.clone(),
                    reason: "timed out waiting for previous session to be deleted".to_owned(),
                });
            }
            Ok(Err(e)) => {
                subtask.failure(None);
                return Err(CliError::PreviewDeleteFailed {
                    name,
                    reason: e.to_string(),
                });
            }
            _ => continue,
        };
    }

    let session_name = PreviewSession::make_resource_name(config_target, key.to_owned());

    // Operators compiled with a custom OPERATOR_ISOLATION_MARKER only reconcile preview
    // sessions labeled with their marker (see the label selector in the preview-env
    // controller). The runtime env var check here allows developers to label the session
    // so it gets picked up by an isolated operator instead of the production one.
    let session_labels = {
        let mut labels = BTreeMap::from([(
            PREVIEW_SESSION_KEY_LABEL.to_owned(),
            EnvKey::to_hashed_label_value(layer_config.key.as_str()),
        )]);
        if let Ok(marker) = std::env::var("OPERATOR_ISOLATION_MARKER") {
            labels.insert(OPERATOR_OWNERSHIP_LABEL.to_owned(), marker);
        }
        labels
    };

    // Label targets with branches were rejected above, so an empty config is the only way a
    // label target gets here and it skips branch preparation entirely.
    let branch_db_names = if layer_config.feature.db_branches.is_empty() {
        BranchDbNames::default()
    } else {
        operator_api
            .prepare_branch_dbs(&layer_config, &progress)
            .await?
    };

    // The namespace the session (and therefore the preview pod) lands in.
    let session_namespace = preview_namespace(&operator_api, &layer_config);

    // Secret mounts never travel on the CR. Their contents are sent to the operator (which creates
    // the backing Secret, naming it after the session) once the session exists; the spec carries
    // only where each key mounts. The TLS client certificate for stolen traffic rides in the same
    // Secret.
    let secret_mounts = resolve_secret_mounts(
        std::mem::take(&mut layer_config.feature.preview.secret_mounts),
        &mut secret_values,
    )?;

    let idle_config = &layer_config.feature.preview.idle;
    let idle = idle_config.is_enabled().then_some(PreviewIdleConfig {
        start_idle: idle_config.start_idle,
        sleep_after_secs: idle_config.sleep_after_secs,
        wake_timeout_secs: idle_config.wake_timeout_secs,
    });

    let session_spec = PreviewSessionSpec {
        image: image.clone(),
        key: layer_config.key.as_str().to_owned(),
        target: session_target,
        ttl_secs: layer_config.feature.preview.resolved_ttl_secs(),
        replicas: layer_config.feature.preview.replicas,
        incoming,
        queue_splitting: PreviewQueueSplittingConfig::from_config(
            &layer_config.feature.split_queues,
        ),
        db_branching: PreviewDbBranchingConfig::from_db_names(branch_db_names),
        env: PreviewEnvVarsConfig::from_config(&layer_config.feature.env).map_err(|error| {
            CliError::EnvFileAccessError(
                layer_config
                    .feature
                    .env
                    .env_file
                    .clone()
                    .unwrap_or_default(),
                error,
            )
        })?,
        labels: PreviewLabelFilter::from_config(&layer_config.feature.preview.labels),
        config_mounts: layer_config
            .feature
            .preview
            .config_mounts
            .into_iter()
            .map(|m| m.resolve().map(Into::into))
            .collect::<Result<Vec<_>, _>>()?,
        secret_mounts,
        idle,
        cronjob,
        spec_resources,
    };

    let annotations = operator_api
        .operator()
        .spec
        .operator_namespace
        .is_some()
        .then(|| {
            let target_ns = layer_config
                .target
                .namespace
                .as_deref()
                .unwrap_or(operator_api.client().default_namespace());
            BTreeMap::from([(TARGET_NAMESPACE_ANNOTATION.to_owned(), target_ns.to_owned())])
        });

    let session = PreviewSession {
        metadata: ObjectMeta {
            name: Some(session_name.clone()),
            labels: Some(session_labels),
            annotations,
            ..Default::default()
        },
        spec: session_spec,
        status: None,
    };

    let session = api
        .create(&PostParams::default(), &session)
        .await
        .map_err(|e| {
            subtask.failure(None);
            CliError::PreviewSessionRejected(e.to_string())
        })?;

    // Now that the session exists we can hand its secret-mount contents to the operator, tagging
    // the Secret with the session's owner reference so it is garbage-collected together with the
    // session. Done after creation so a rejected session never leaves an orphaned Secret.
    if !secret_values.is_empty() {
        let owner_ref = session.owner_ref(&()).ok_or_else(|| {
            subtask.failure(None);
            CliError::PreviewSecretMountFailed("created session is missing a UID".to_owned())
        })?;

        if let Err(error) = operator_api
            .create_preview_secret_mounts(&session_namespace, owner_ref, secret_values)
            .await
        {
            // The session can never become ready without its Secret, so remove it.
            let _ = api.delete(&session_name, &DeleteParams::default()).await;
            subtask.failure(None);
            return Err(CliError::PreviewSecretMountFailed(error.to_string()));
        }
    }

    subtask.success(Some("preview session resource created"));

    // Watch the `PreviewSession` status until it reaches `Ready` or `Failed`. Emits
    // periodic warnings if initialization is taking longer than expected, so the user knows
    // the command hasn't hung. If the session does not become `Ready` within the timeout,
    // the CLI deletes the session resource.

    let mut subtask = progress.subtask("waiting for preview to be ready");

    let mut stream = std::pin::pin!(watcher(
        api.clone(),
        watcher::Config::default().fields(&format!("metadata.name={}", session.name_any())),
    ));

    let initialization_start = Instant::now();
    let mut long_initialization_timer = tokio::time::interval(Duration::from_secs(60));
    // First tick is instant
    long_initialization_timer.tick().await;

    let mut timeout = std::pin::pin!(tokio::time::sleep(Duration::from_secs(
        layer_config.feature.preview.creation_timeout_secs,
    )));

    let mut last_known_phase: &str = "unknown";
    // Assigned exactly once, in the `Ready` arm below, which is the only path that leaves the loop.
    let share_host;

    loop {
        tokio::select! {
            _ = &mut timeout => {
                // Read the pods before deleting the session: the delete tears down the
                // deployment, and their output goes with it.
                let logs = fetch_preview_logs_best_effort(
                    operator_api.client(),
                    &session_namespace,
                    &session.name_any(),
                )
                .await;

                if let Err(err) = delete::delete_and_finalize(api, &session.name_any(), &DeleteParams::default()).await {
                    subtask.warning(&format!(
                        "failed to delete timed out session '{}': {err}, \
                         you may need to delete it manually or with `mirrord preview stop`",
                        session.name_any(),
                    ));
                }

                subtask.failure(None);
                return Err(CliError::PreviewTimeout { logs });
            }
            _ = long_initialization_timer.tick() => {
                subtask.warning(&format!(
                    "preview initialization is taking over {}s, phase: {}",
                    initialization_start.elapsed().as_secs(),
                    last_known_phase
                ));
            }
            event = stream.next() => {
                match event {
                    Some(Ok(Event::Apply(current) | Event::InitApply(current))) => {
                        if let Some(status) = &current.status {
                            match &status.phase {
                                PreviewSessionPhase::Initializing => {
                                    last_known_phase = "initializing preview env";
                                }
                                PreviewSessionPhase::Waiting => {
                                    last_known_phase = "waiting for preview pod to be ready";
                                }
                                PreviewSessionPhase::Ready => {
                                    share_host = status.share_host.clone();
                                    subtask.success(Some("preview session is ready"));
                                    break;
                                }
                                // Sessions started with `feature.preview.idle.start_idle` never
                                // pass through `Ready` on creation — `Idle` is their terminal
                                // success state (pods boot on first traffic).
                                PreviewSessionPhase::Idle => {
                                    share_host = status.share_host.clone();
                                    subtask.success(Some(
                                        "preview session is idle (pods will boot on first traffic)",
                                    ));
                                    break;
                                }
                                PreviewSessionPhase::Failed => {
                                    let failure_message = status.failure_message.clone().expect("Failed session must have failure_message");
                                    let logs = fetch_preview_logs_best_effort(
                                        operator_api.client(),
                                        &session_namespace,
                                        &session.name_any(),
                                    )
                                    .await;

                                    subtask.failure(None);
                                    return Err(CliError::PreviewSessionFailed {
                                        message: failure_message,
                                        logs,
                                    });
                                }
                                PreviewSessionPhase::Paused => {
                                    last_known_phase = "preview session is paused";
                                }
                                PreviewSessionPhase::Unknown => last_known_phase = "unknown",
                            }
                        }
                    }

                    Some(Ok(Event::Delete(_))) => {
                        subtask.failure(None);
                        return Err(CliError::PreviewSessionDeleted);
                    }

                    Some(Ok(Event::Init | Event::InitDone)) => continue,

                    Some(Err(error)) => {
                        subtask.failure(None);
                        return Err(CliError::PreviewWatchFailed(error.to_string()));
                    }

                    None => {
                        subtask.failure(None);
                        return Err(CliError::PreviewWatchFailed("stream closed".to_owned()));
                    }
                }
            }
        }
    }

    // Display summary of the created preview environment.

    let namespace = layer_config
        .target
        .namespace
        .as_deref()
        .unwrap_or(operator_api.client().default_namespace());

    // On multicluster, `Ready` above reflects the default cluster; the other clusters'
    // replicas converge on their own. Wait for them (bounded) so "ready" means ready
    // EVERYWHERE, without letting one dead cluster hold the start hostage.
    let outcome = multicluster::wait_for_replica_clusters(
        operator_api.client().clone(),
        namespace,
        &session_name,
        &mut progress,
    )
    .await;
    match outcome {
        multicluster::ReplicaOutcome::Live => {}
        // The preview is gone: reporting a successful start would print a key and session
        // name for something the user cannot use.
        multicluster::ReplicaOutcome::Failed(message) => {
            let logs = fetch_preview_logs_best_effort(
                operator_api.client(),
                &session_namespace,
                &session_name,
            )
            .await;

            progress.failure(None);
            return Err(CliError::PreviewSessionFailed { message, logs });
        }
        multicluster::ReplicaOutcome::Deleted => {
            progress.failure(None);
            return Err(CliError::PreviewSessionDeleted);
        }
    }

    progress.success(Some("preview environment created successfully"));

    let key = layer_config.key.as_str();

    // This line is parsed by the github action to generate an output,
    // so please update it as well if you're gonna change this line.
    // We're doing this weird .subtask().success() stuff because
    // otherwise it messes up the ordering or looks weird in some
    // other way :'(
    progress.subtask(&format!("key: {key}")).success(None);
    progress
        .subtask(&format!("namespace: {namespace}"))
        .success(None);
    progress
        .subtask(&format!("session: {session_name}"))
        .success(None);

    if let Some(share_host) = share_host {
        progress
            .subtask(&format!("preview URL: https://{share_host}"))
            .success(None);
    }

    Ok(())
}

/// Handle `mirrord preview status` command.
///
/// Lists preview environments, optionally filtered by key, namespace, and whether failed
/// sessions should be shown.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_status(
    common: &PreviewCommonArgs,
    args: PreviewStatusArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview status");

    let layer_config = load_preview_config(args.as_env_vars(common), &mut progress).await?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch.clone(),
        user_data.machine_id(),
        Some(layer_config.key.as_str().to_owned()),
    );

    let (operator_api, api) = create_preview_api(
        &layer_config,
        args.all_namespaces,
        &progress,
        &mut analytics,
    )
    .await?;

    // List and filter sessions.

    let mut subtask = progress.subtask("listing preview sessions");

    let matcher = match args.glob.as_deref() {
        Some(glob) => KeyMatcher::Glob(glob),
        None => match layer_config.key.provided() {
            Some(key) => KeyMatcher::Simple(key),
            None => KeyMatcher::Any,
        },
    };

    let sessions = matcher
        .list_matching_sessions(&api)
        .await
        .inspect_err(|_| subtask.failure(None))?;

    let sessions: Vec<_> = sessions
        .iter()
        .filter(|session| {
            let Some(status) = session.status.as_ref() else {
                return false;
            };

            // Older operators reused the `Failed` phase with a specific failure message
            // when the preview TTL elapsed instead of deleting them, so we need to handle
            // that to hide expired preview sessions in a backwards compatible way.
            // See https://github.com/metalbear-co/operator/blob/17e4c645d59affefc672f597a10e2880c405f043/crates/operator-preview-env/src/task.rs#L774-L775
            let (failed, expired) = match (status.phase, status.failure_message.as_deref()) {
                (PreviewSessionPhase::Failed, Some("preview session TTL expired")) => (false, true),
                (PreviewSessionPhase::Failed, _) => (true, false),
                _ => (false, false),
            };

            if args.failed {
                failed
            } else {
                // Not failed and not expired = alive
                !failed && !expired
            }
        })
        .collect();

    if sessions.is_empty() {
        subtask.success(Some("no preview sessions found"));
        progress.success(None);
        return Ok(());
    }

    subtask.success(Some(&format!(
        "found {} session{}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    )));

    progress.success(None);

    // One previews-API call per session backs the multicluster detail below; they run
    // concurrently so the command costs one round trip rather than one per session.
    let views = multicluster::cluster_views(operator_api.client(), &sessions).await;

    // Display sessions ordered by key.

    let mut sessions_by_key: BTreeMap<&str, Vec<&PreviewSession>> = BTreeMap::new();

    for session in &sessions {
        sessions_by_key
            .entry(session.spec.key.as_str())
            .or_default()
            .push(session);
    }

    let mut table = Table::new();
    table.add_row(row![
        "Key",
        "Session ID",
        "Target",
        "Namespace",
        "Status",
        "Clusters",
        "Message"
    ]);

    for (key, sessions) in sessions_by_key {
        for session in sessions.iter() {
            let session_name = session.metadata.name.as_deref().unwrap_or("<unknown>");

            let status = match session.status.as_ref().map(|status| status.phase) {
                Some(PreviewSessionPhase::Initializing) => "initializing".to_owned(),
                Some(PreviewSessionPhase::Waiting) => "waiting".to_owned(),
                Some(PreviewSessionPhase::Ready) => {
                    if session.spec.has_infinite_ttl() {
                        "running (infinite)".to_owned()
                    } else {
                        let remaining = session
                            .status
                            .as_ref()
                            .and_then(|s| s.expires_at.as_ref())
                            .and_then(|expires_at| {
                                Duration::try_from(expires_at.0.duration_since(Timestamp::now()))
                                    .ok()
                            })
                            .map(|d| Duration::from_secs(d.as_secs()));
                        match remaining {
                            Some(d) => {
                                format!("running ({} remaining)", humantime::format_duration(d))
                            }
                            None => "running".to_owned(),
                        }
                    }
                }
                Some(PreviewSessionPhase::Failed) => session
                    .status
                    .as_ref()
                    .and_then(|status| status.failure_message.as_deref())
                    .unwrap_or("unknown")
                    .to_owned(),
                Some(PreviewSessionPhase::Idle) => "idle (waiting for traffic)".to_owned(),
                Some(PreviewSessionPhase::Paused) => "paused".to_owned(),
                Some(PreviewSessionPhase::Unknown) => "unknown".to_owned(),
                None => "pending".to_owned(),
            };

            // Multicluster detail from the previews view, best-effort (older operators do
            // not serve it): the per-cluster phases `preview start` waited on, and any
            // replica degradation - so `status` can actually re-check what `start` reported.
            let (clusters, message) = session
                .metadata
                .namespace
                .as_deref()
                .and_then(|namespace| views.get(&(namespace.to_owned(), session_name.to_owned())))
                .map(|view_status| {
                    let clusters = view_status
                        .clusters
                        .iter()
                        .map(|(cluster, status)| {
                            format!("{cluster}: {}", status.phase.to_string().to_lowercase())
                        })
                        .join(", ");

                    let message = view_status
                        .message
                        .as_ref()
                        .map(|message| {
                            let label = match message.kind {
                                PreviewMessageKind::Failure => "failure",
                                PreviewMessageKind::Degraded => "degraded",
                                PreviewMessageKind::Unknown => "unknown",
                            };
                            format!("{label}: {}", message.text)
                        })
                        .unwrap_or_default();

                    (clusters, message)
                })
                .unwrap_or_default();

            table.add_row(row![
                key,
                session_name,
                session.spec.target,
                session.metadata.namespace.as_deref().unwrap_or_default(),
                status,
                clusters,
                message
            ]);

            if let Some(license_fingerprint) =
                operator_api.operator().spec.license.fingerprint.as_deref()
            {
                PreviewEvent::new(
                    &session.spec.key,
                    license_fingerprint,
                    session.runtime_secs(),
                    PreviewEventKind::Status,
                )
                .cli_report_analytics(
                    layer_config.telemetry,
                    watch.clone(),
                    user_data.machine_id(),
                );
            }
        }
    }

    table.printstd();

    Ok(())
}

/// Handle `mirrord preview logs` command.
///
/// Prints what the preview pods of every matching environment have written. Unlike
/// `preview status`, failed environments are included: the output of one that died is the
/// main thing worth reading, and it stays available for as long as the operator retains the
/// failed session.
async fn preview_logs(
    common: &PreviewCommonArgs,
    args: PreviewLogsArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview logs");

    let layer_config = load_preview_config(args.as_env_vars(common), &mut progress).await?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch.clone(),
        user_data.machine_id(),
        Some(layer_config.key.as_str().to_owned()),
    );

    let (operator_api, api) = create_preview_api(
        &layer_config,
        args.all_namespaces,
        &progress,
        &mut analytics,
    )
    .await?;

    let mut subtask = progress.subtask("listing preview sessions");

    let matcher = match args.glob.as_deref() {
        Some(glob) => KeyMatcher::Glob(glob),
        None => match layer_config.key.provided() {
            Some(key) => KeyMatcher::Simple(key),
            None => KeyMatcher::Any,
        },
    };

    // Reading every environment that shares a key is rarely what someone wants; a target
    // narrows it to the one they are actually debugging.
    let session_target = match &layer_config.target.path {
        Some(config_target) => Some(
            resolve_config_target(
                config_target,
                operator_api.client(),
                layer_config.target.namespace.as_deref(),
            )
            .await
            .inspect_err(|_| subtask.failure(None))?,
        ),
        None => None,
    };

    let sessions: Vec<_> = matcher
        .list_matching_sessions(&api)
        .await
        .inspect_err(|_| subtask.failure(None))?
        .into_iter()
        .filter(|session| {
            session_target
                .as_ref()
                .is_none_or(|target| session.spec.target == *target)
        })
        .collect();

    // Silence would be ambiguous here: this command's only output is the logs themselves, so
    // finding nothing has to say so. `preview stop` treats an empty match the same way.
    if sessions.is_empty() {
        subtask.failure(None);
        return Err(CliError::PreviewNotFound(matcher.as_str().to_owned()));
    }

    subtask.success(Some(&format!(
        "found {} session{}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    )));
    progress.success(None);

    let sessions: Vec<&PreviewSession> = sessions.iter().collect();
    print_session_logs(operator_api.client(), &sessions)
        .await
        .map_err(|error| CliError::PreviewLogsFailed(error.to_string()))?;

    Ok(())
}

/// Prints each session's pod output, for the sessions that produced any.
///
/// A read that FAILS is reported rather than skipped: printing nothing for it would be
/// indistinguishable from a preview whose pods stayed quiet, and this command exists to answer
/// exactly that question. Reads run concurrently: one round trip for the whole command rather
/// than one per session, matching how `preview status` fetches its multicluster detail.
async fn print_session_logs(
    client: &Client,
    sessions: &[&PreviewSession],
) -> Result<(), kube::Error> {
    let reads = sessions.iter().filter_map(|session| {
        let name = session.metadata.name.as_deref()?;
        let namespace = session.metadata.namespace.as_deref()?;

        Some(async move {
            (
                session.spec.key.as_str(),
                name,
                fetch_preview_logs(client, namespace, name).await,
            )
        })
    });

    for (key, name, logs) in futures::future::join_all(reads).await {
        let rendered = format_preview_logs(&logs?);
        if rendered.is_empty() {
            continue;
        }

        println!("\n{key} ({name}){rendered}");
    }

    Ok(())
}

/// Handle `mirrord preview stop` command.
///
/// Deletes preview environments matching the given key and, optionally, a target filter and
/// namespace.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_stop(
    common: &PreviewCommonArgs,
    args: PreviewStopArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview stop");

    let layer_config = load_preview_config(args.as_env_vars(common), &mut progress).await?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch,
        user_data.machine_id(),
        Some(layer_config.key.as_str().to_owned()),
    );

    let matcher = match args.glob.as_deref() {
        Some(glob) => KeyMatcher::Glob(glob),
        None => KeyMatcher::Simple(
            layer_config
                .key
                .provided()
                .ok_or(CliError::SessionKeyRequired)?,
        ),
    };

    let (operator_api, api) = create_preview_api(
        &layer_config,
        args.all_namespaces,
        &progress,
        &mut analytics,
    )
    .await?;

    let mut subtask = progress.subtask("finding preview sessions");

    let session_target = match &layer_config.target.path {
        Some(config_target) => Some(
            resolve_config_target(
                config_target,
                operator_api.client(),
                layer_config.target.namespace.as_deref(),
            )
            .await
            .inspect_err(|_| subtask.failure(None))?,
        ),
        None => None,
    };

    let sessions_to_delete: Vec<_> = matcher
        .list_matching_sessions(&api)
        .await
        .inspect_err(|_| subtask.failure(None))?
        .into_iter()
        .filter(|session| {
            session_target
                .as_ref()
                .is_none_or(|target| session.spec.target == *target)
        })
        .collect();

    if sessions_to_delete.is_empty() {
        subtask.failure(None);
        return Err(CliError::PreviewNotFound(matcher.as_str().to_owned()));
    }

    subtask.success(Some(&format!(
        "found {} session{} to delete",
        sessions_to_delete.len(),
        if sessions_to_delete.len() == 1 {
            ""
        } else {
            "s"
        }
    )));

    // Delete all matching sessions.

    let mut delete_subtask = progress.subtask("deleting sessions");

    let mut result = Ok(());
    for session in sessions_to_delete {
        let name = session
            .metadata
            .name
            .as_deref()
            .expect("preview session should have a name");
        let namespace = session
            .metadata
            .namespace
            .as_deref()
            .expect("preview session should have a namespace");

        let namespaced_api =
            Api::<PreviewSession>::namespaced(operator_api.client().clone(), namespace);

        if let Err(e) =
            delete::delete_and_finalize(namespaced_api.clone(), name, &DeleteParams::default())
                .await
        {
            result = Err(CliError::PreviewDeleteFailed {
                name: name.to_owned(),
                reason: e.to_string(),
            });
        }
    }

    if result.is_err() {
        delete_subtask.failure(None);
        return result;
    }

    delete_subtask.success(Some("all sessions deleted"));
    progress.success(None);

    Ok(())
}

/// Handle `mirrord preview diff` command.
///
/// Runs the `--resource` checks of `preview start` (scope, comparison with the live cluster,
/// server-side dry runs) and prints how each object in scope differs. Creates nothing.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_diff(
    common: &PreviewCommonArgs,
    args: PreviewDiffArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview diff");

    let mut layer_config = load_preview_config(args.as_env_vars(common), &mut progress).await?;

    let resource_paths = resource_paths(args.resources, &mut layer_config);
    if resource_paths.is_empty() {
        return Err(CliError::PreviewResourcesRequired);
    }
    let mut objects = load_manifests(&resource_paths, &progress)?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch,
        user_data.machine_id(),
        Some(layer_config.key.as_str().to_owned()),
    );

    let (operator_api, _) =
        create_preview_api(&layer_config, false, &progress, &mut analytics).await?;
    // Nothing is sent to the operator, so an operator without `PreviewSpecResources` can still
    // show what would change.
    reject_management_only(&operator_api.operator().spec)?;

    let mut subtask = progress.subtask("comparing manifests with the cluster");

    let config_target = layer_config.target.path.as_ref().ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewTargetRequired
    })?;

    let session_target = resolve_config_target(
        config_target,
        operator_api.client(),
        layer_config.target.namespace.as_deref(),
    )
    .await
    .inspect_err(|_| subtask.failure(None))?;
    let session_target =
        resources_target(&session_target).inspect_err(|_| subtask.failure(None))?;

    resources::resolve_workload_refs(
        operator_api.client(),
        &mut objects,
        session_target,
        target_namespace(&operator_api, &layer_config),
    )
    .await
    .inspect_err(|_| subtask.failure(None))?;

    let plan = resources::plan(
        operator_api.client(),
        &objects,
        resources::sources_label(&resource_paths),
        session_target,
        target_namespace(&operator_api, &layer_config),
    )
    .await
    .inspect_err(|_| subtask.failure(None))?;

    for note in &plan.notes {
        subtask.warning(note);
    }
    subtask.success(Some(
        "compared manifests with the cluster, nothing was created",
    ));
    progress.success(None);

    print!("{}", resources::render_diff(&plan));

    Ok(())
}

/// The manifest paths for `--resource`. Paths on the command line replace the config's list
/// instead of adding to it, so a CI job can point one shared config at the manifests of the
/// change it is previewing.
fn resource_paths(from_args: Vec<PathBuf>, config: &mut LayerConfig) -> Vec<PathBuf> {
    let from_config = std::mem::take(&mut config.feature.preview.spec_resources);
    if from_args.is_empty() {
        from_config
    } else {
        from_args
    }
}

fn load_manifests(
    paths: &[PathBuf],
    progress: &ProgressTracker,
) -> CliResult<Vec<resources::SuppliedObject>> {
    let mut subtask = progress.subtask("reading manifests");
    let objects = resources::load(paths).inspect_err(|_| subtask.failure(None))?;
    subtask.success(Some(&format!(
        "read {} object{} from {}",
        objects.len(),
        if objects.len() == 1 { "" } else { "s" },
        resources::sources_label(paths),
    )));
    Ok(objects)
}

/// `--resource` compares against and builds from objects in the target's cluster, which the
/// CLI reaches with the user's own credentials. A management-only operator lives in a cluster
/// without the target, and an operator without `PreviewSpecResources` would drop the field
/// the files travel in and silently run the live spec: both are refused up front.
fn require_spec_resources_support(spec: &MirrordOperatorSpec) -> CliResult<()> {
    spec.require_feature(NewOperatorFeature::PreviewSpecResources)?;
    reject_management_only(spec)
}

fn reject_management_only(spec: &MirrordOperatorSpec) -> CliResult<()> {
    if spec.operator_namespace.is_some() {
        return Err(CliError::PreviewResourcesManagementOnly);
    }
    Ok(())
}

/// `--resource` compares the files with one workload's live pod template, and a label target
/// matches pods of any number of workloads.
fn resources_target(target: &SessionTarget) -> CliResult<&KubeResourceTarget> {
    match target {
        SessionTarget::KubeResource(target) => Ok(target),
        SessionTarget::PodSet(_) => Err(CliError::UnsupportedTargetConfig(format!(
            "`--resource` does not support label target `{}`; target a single workload, or leave \
             out `--resource`",
            target.display_name()
        ))),
    }
}

/// The target's namespace: the configured one, or the kubeconfig default.
fn target_namespace<'a>(
    operator_api: &'a OperatorApi<NoClientCert>,
    config: &'a LayerConfig,
) -> &'a str {
    config
        .target
        .namespace
        .as_deref()
        .unwrap_or(operator_api.client().default_namespace())
}

/// Resolves a [`Target`] to a [`SessionTarget`] by fetching the target from the operator's
/// GET TargetCrd API. The operator validates the target exists and resolves the container if
/// not specified. Works for both single-cluster and multi-cluster.
///
/// Falls back to local `runtime_data` resolution if the operator didn't resolve the
/// container (backwards compatibility with older operators).
///
/// A label target is sent as written: it has no single workload for the operator to fetch,
/// and the pods it matches can each name their container differently. The operator reports
/// a selector that matches no ready pod as a failed session.
async fn resolve_config_target(
    config_target: &Target,
    client: &kube::Client,
    namespace: Option<&str>,
) -> CliResult<SessionTarget> {
    if let Target::Label(label_target) = config_target {
        return Ok(label_session_target(label_target));
    }

    let ns = namespace.unwrap_or(client.default_namespace());
    let target_api: Api<TargetCrd> = Api::namespaced(client.clone(), ns);
    let target_crd = target_api
        .get(&TargetCrd::urlfied_name(config_target))
        .await
        .map_err(|e| CliError::PreviewTargetResolutionFailed(e.to_string()))?;
    let mut target = target_crd
        .spec
        .target
        .as_known()
        .map_err(|e| CliError::PreviewTargetResolutionFailed(e.to_string()))?
        .clone();

    // Older operators don't resolve the container in GET TargetCrd. Fall back to
    // querying the cluster directly so the CLI stays compatible with them.
    if target.container().is_none() {
        let runtime_data = config_target
            .runtime_data(client, namespace)
            .await
            .map_err(|e| CliError::PreviewTargetResolutionFailed(e.to_string()))?;
        target.set_container(runtime_data.container_name);
    }

    SessionTarget::from_config(target).ok_or_else(|| {
        CliError::PreviewTargetResolutionFailed("no valid container found".to_owned())
    })
}

/// Builds the session target for a label target. An empty container tells the operator to
/// pick the container in each matching pod on its own.
fn label_session_target(label_target: &LabelTarget) -> SessionTarget {
    SessionTarget::PodSet(PodSetTarget::new(
        label_target.labels.clone().into_iter().collect(),
        label_target.container.clone().unwrap_or_default(),
    ))
}

async fn load_preview_config(
    env_overrides: HashMap<&OsStr, Cow<'_, OsStr>>,
    progress: &mut ProgressTracker,
) -> CliResult<LayerConfig> {
    let mut subtask = progress.subtask("loading configuration");

    let mut cfg_context = ConfigContext::default().override_envs(env_overrides);

    let config = crate::util::resolve_layer_config(&mut cfg_context)
        .await
        .inspect_err(|_| {
            subtask.failure(None);
        })?;

    let result = config.verify_for_preview_env(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        subtask.warning(&warning);
    }
    result?;

    subtask.success(Some("configuration loaded"));

    Ok(config)
}

#[derive(Clone, Copy)]
enum KeyMatcher<'a> {
    Simple(&'a str),
    Glob(&'a str),
    Any,
}

impl KeyMatcher<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Simple(key) => key,
            Self::Glob(key) => key,
            Self::Any => "<any>",
        }
    }

    async fn list_matching_sessions(
        self,
        api: &Api<PreviewSession>,
    ) -> CliResult<Vec<PreviewSession>> {
        let sessions = match self {
            Self::Simple(key) => {
                let key_label = EnvKey::to_hashed_label_value(key);

                // Older CLIs stored the raw key in this label, so when the raw key is a valid label
                // value we include both forms in a set selector. Invalid raw keys must not be
                // included in the selector: the API server rejects selectors
                // containing invalid label values instead of treating them as
                // non-matching values. Those keys can only match sessions
                // created by newer CLIs, since older CLIs could not create resources with invalid
                // label values in the first place.
                let label_selector = if EnvKey::is_valid_kubernetes_label_value(key) {
                    format!("{PREVIEW_SESSION_KEY_LABEL} in ({key},{key_label})")
                } else {
                    format!("{PREVIEW_SESSION_KEY_LABEL}={key_label}")
                };

                api.list(&ListParams {
                    label_selector: Some(label_selector),
                    ..Default::default()
                })
                .await
                .map(|sessions| sessions.items)
            }
            Self::Any => api
                .list(&ListParams::default())
                .await
                .map(|sessions| sessions.items),
            Self::Glob(glob) => {
                let pattern = Pattern::new(glob).map_err(|error| {
                    CliError::PreviewListFailed(format!("invalid key glob `{glob}`: {error}"))
                })?;

                api.list(&ListParams::default()).await.map(|sessions| {
                    sessions
                        .items
                        .into_iter()
                        .filter(|session| pattern.matches(&session.spec.key))
                        .collect()
                })
            }
        };

        sessions.map_err(|error| CliError::PreviewListFailed(error.to_string()))
    }
}

/// Connects to the operator, validates the license and checks that the `PreviewEnv` feature is
/// supported, then returns the operator API and a `PreviewSession` API handle scoped to the
/// appropriate namespace(s).
/// Resolves the configured secret mounts. The raw file contents go into `secret_values` keyed
/// `k0`, `k1`, ... for the operator's `previewsecretmounts` endpoint; the returned per-file
/// references (path + `Secret` key) go on the `PreviewSession` spec. The `Secret` name is in
/// neither - the operator derives it from the session name.
fn resolve_secret_mounts(
    mounts: Vec<ConfigMount>,
    secret_values: &mut BTreeMap<String, ByteString>,
) -> CliResult<Vec<PreviewSecretMountFile>> {
    let mut files = Vec::with_capacity(mounts.len());

    for (index, mount) in mounts.into_iter().enumerate() {
        let resolved = mount.resolve()?;
        let key = format!("k{index}");

        // `resolve` hands back verbatim UTF-8 for text and base64 for binary; the Secret needs the
        // raw bytes either way, and Kubernetes base64-encodes them again on the wire.
        let bytes = match resolved.r#type {
            Some(ConfigMountType::Binary) => BASE64_STANDARD
                .decode(resolved.payload.unwrap_or_default())
                .map_err(|error| {
                    CliError::PreviewSecretMountFailed(format!("decoding {key}: {error}"))
                })?,
            _ => resolved.payload.unwrap_or_default().into_bytes(),
        };

        secret_values.insert(key.clone(), ByteString(bytes));
        files.push(PreviewSecretMountFile {
            path: resolved.mount_at,
            secret_key: key,
        });
    }

    Ok(files)
}

/// Resolves the TLS delivery settings a preview honors. The operator makes the TLS connection
/// to the preview pod, so a local client certificate and its key are read here and stored in
/// `secret_values` for the session's Secret; the CR only names their keys. With
/// `client_cert_source: target` the files stay in the cluster and the CR carries their
/// in-container paths for the operator to read. Also returns a warning for every configured
/// setting a preview cannot honor.
fn resolve_tls_delivery(
    config: Option<&LocalTlsDelivery>,
    secret_values: &mut BTreeMap<String, ByteString>,
    operator: &MirrordOperatorSpec,
) -> CliResult<(Option<PreviewTlsDelivery>, Vec<String>)> {
    let Some(config) = config else {
        return Ok((None, Vec::new()));
    };

    // Previews deliver over TLS even when exec would ignore these fields for TCP.
    if config.client_cert.is_some() != config.client_key.is_some() {
        return Err(ConfigError::Conflict(
            ".feature.network.incoming.tls_delivery.client_cert and \
             .feature.network.incoming.tls_delivery.client_key must be set together"
                .to_owned(),
        )
        .into());
    }
    // Exec skips a target source with no files under `protocol: tcp`, because it
    // never opens TLS. A preview still does, and would otherwise start with no
    // client certificate for the operator to present.
    if config.client_cert_source == TlsClientCertSource::Target && config.client_cert.is_none() {
        return Err(ConfigError::Conflict(
            ".feature.network.incoming.tls_delivery.client_cert_source is `target` \
             but .feature.network.incoming.tls_delivery.client_cert and \
             .feature.network.incoming.tls_delivery.client_key are not set"
                .to_owned(),
        )
        .into());
    }
    if config.client_cert.is_some() || config.server_name.is_some() {
        operator.require_feature(NewOperatorFeature::PreviewTlsDelivery)?;
    }
    if config.client_cert_source == TlsClientCertSource::Target {
        operator.require_feature(NewOperatorFeature::PreviewTlsClientAuthFromTarget)?;
    }

    let mut warnings = Vec::new();
    if config.protocol == TlsDeliveryProtocol::Tcp {
        warnings.push(
            "`feature.network.incoming.tls_delivery.protocol: tcp` does not apply to previews: \
            the operator always delivers stolen TLS traffic to the preview pod over TLS"
                .to_owned(),
        );
    }
    if config.trust_roots.is_some() || config.server_cert.is_some() {
        warnings.push(
            "`feature.network.incoming.tls_delivery.trust_roots` and `server_cert` do not apply \
            to previews: the operator does not verify the preview pod's certificate"
                .to_owned(),
        );
    }

    let mut client_auth = None;
    let mut client_auth_from_target = None;
    match (
        config.client_cert.as_deref(),
        config.client_key.as_deref(),
        config.client_cert_source,
    ) {
        (Some(cert), Some(key), TlsClientCertSource::Local) => {
            secret_values.insert(
                PreviewTlsClientAuth::CERT_SECRET_KEY.to_owned(),
                ByteString(read_tls_client_auth_file(cert)?),
            );
            secret_values.insert(
                PreviewTlsClientAuth::KEY_SECRET_KEY.to_owned(),
                ByteString(read_tls_client_auth_file(key)?),
            );
            client_auth = Some(PreviewTlsClientAuth {
                cert_secret_key: PreviewTlsClientAuth::CERT_SECRET_KEY.to_owned(),
                key_secret_key: PreviewTlsClientAuth::KEY_SECRET_KEY.to_owned(),
            });
        }
        // Config paths come from JSON, so they are UTF-8 and the lossy conversion is exact.
        (Some(cert), Some(key), TlsClientCertSource::Target) => {
            client_auth_from_target = Some(PreviewTlsClientAuthFromTarget {
                cert_path: cert.to_string_lossy().into_owned(),
                key_path: key.to_string_lossy().into_owned(),
            });
        }
        // Pairing was checked above, including for TCP configuration.
        _ => {}
    }

    let tls_delivery = PreviewTlsDelivery {
        server_name: config.server_name.clone(),
        client_auth,
        client_auth_from_target,
    };
    // Nothing to honor: leave the CR identical to what older CLIs send.
    let tls_delivery = (tls_delivery != PreviewTlsDelivery::default()).then_some(tls_delivery);

    Ok((tls_delivery, warnings))
}

fn read_tls_client_auth_file(path: &Path) -> CliResult<Vec<u8>> {
    std::fs::read(path).map_err(|error| CliError::PreviewTlsClientAuthFile {
        path: path.to_path_buf(),
        error,
    })
}

async fn create_preview_api(
    config: &LayerConfig,
    all_namespaces: bool,
    progress: &ProgressTracker,
    analytics: &mut AnalyticsReporter,
) -> CliResult<(OperatorApi<NoClientCert>, Api<PreviewSession>)> {
    let mut subtask = progress.subtask("connecting to operator");

    let operator_api = OperatorApi::try_new(config, analytics, progress)
        .await?
        .ok_or_else(|| {
            subtask.failure(None);
            analytics
                .get_mut()
                .add_operator_wall(OperatorWall::PreviewCommand);
            analytics.set_error(AnalyticsError::Unknown);
            CliError::OperatorNotInstalled
        })?;

    operator_api
        .check_license_validity(progress)
        .inspect_err(|_| {
            analytics
                .get_mut()
                .add_operator_wall(OperatorWall::LicenseExpired);
            analytics.set_error(AnalyticsError::Unknown);
        })?;

    operator_api
        .operator()
        .spec
        .require_feature(NewOperatorFeature::PreviewEnv)
        .inspect_err(|_| {
            subtask.failure(None);
        })?;

    subtask.success(Some("connected to operator"));

    let client = operator_api.client().clone();

    let api = if all_namespaces {
        Api::all(client)
    } else {
        Api::namespaced(client, &preview_namespace(&operator_api, config))
    };

    Ok((operator_api, api))
}

/// Resolves the namespace a preview session and its resources live in.
///
/// First match wins:
/// - the operator's own namespace (management-only / centralized operators)
/// - the target's namespace from the mirrord config
/// - the kubeconfig default namespace
fn preview_namespace(operator_api: &OperatorApi<NoClientCert>, config: &LayerConfig) -> String {
    operator_api
        .operator()
        .spec
        .operator_namespace
        .as_deref()
        .or(config.target.namespace.as_deref())
        .unwrap_or_else(|| operator_api.client().default_namespace())
        .to_owned()
}

/// Reads the preview's pod output through the operator's `logs` subresource.
///
/// The single read path for every failure: the operator answers from the tail it stored when
/// it gave up on the session, or tails the pods live when it has not stored one yet. Reading
/// the CR's own `failureLogs` instead would race the operator, which stores that tail on a
/// later reconcile than the one that marks the session failed.
///
/// Best-effort by design: this runs while a start is already failing, so an operator without
/// the route, a lost connection, or pods that are simply gone all resolve to no logs rather
/// than replacing the failure the user actually needs to see.
async fn fetch_preview_logs(
    client: &Client,
    namespace: &str,
    session_name: &str,
) -> Result<Vec<PreviewPodLogs>, kube::Error> {
    Api::<PreviewEnv>::namespaced(client.clone(), namespace)
        .get_subresource("logs", session_name)
        .await
        .map(|view: PreviewEnv| view.status.map(|status| status.logs).unwrap_or_default())
}

/// The best-effort form, for the paths that are already reporting a different failure.
///
/// A start that is going to fail anyway must not have its own error replaced by one about
/// reading logs, so an unreachable route, a lost connection, or pods that are simply gone all
/// resolve to no logs here. `mirrord preview logs` does NOT use this: there, a failed read is
/// the only thing worth saying.
async fn fetch_preview_logs_best_effort(
    client: &Client,
    namespace: &str,
    session_name: &str,
) -> Vec<PreviewPodLogs> {
    fetch_preview_logs(client, namespace, session_name)
        .await
        .inspect_err(|error| tracing::debug!(%error, "failed to read preview pod logs"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The operator rebuilds the config target from the session target. A label target with no
    /// container must come back with no container, so each matching pod picks its own instead
    /// of all pods being required to have a container the user never named.
    #[test]
    fn label_target_without_container_reaches_the_operator_unchanged() {
        let label_target = LabelTarget {
            labels: BTreeMap::from([("app".to_owned(), "checkout".to_owned())]),
            container: None,
        };

        assert_eq!(
            label_session_target(&label_target).into_config(),
            Some(Target::Label(label_target))
        );
    }

    fn operator(supports_tls: bool) -> MirrordOperatorSpec {
        if supports_tls {
            operator_with(&[NewOperatorFeature::PreviewTlsDelivery])
        } else {
            operator_with(&[])
        }
    }

    /// An operator supporting previews plus the given features.
    fn operator_with(extra_features: &[NewOperatorFeature]) -> MirrordOperatorSpec {
        let mut features = vec![NewOperatorFeature::PreviewEnv];
        features.extend_from_slice(extra_features);
        serde_json::from_value(serde_json::json!({
            "operator_version": "3.211.0",
            "default_namespace": "default",
            "supported_features": features,
            "license": {"name": "test", "organization": "test", "expire_at": "2099-01-01"}
        }))
        .unwrap()
    }

    /// An operator without `PreviewSpecResources` would have the API server prune
    /// `spec.specResources` from the session and run the live spec, silently ignoring the
    /// user's files. The CLI refuses instead.
    #[test]
    fn old_operator_refuses_resource_instead_of_ignoring_it() {
        let error = require_spec_resources_support(&operator(false)).unwrap_err();
        assert!(
            matches!(&error, CliError::FeatureNotSupportedInOperatorError { feature, .. } if *feature == NewOperatorFeature::PreviewSpecResources.to_string()),
            "{error}"
        );

        let mut supported: MirrordOperatorSpec = serde_json::from_value(serde_json::json!({
            "operator_version": "3.213.0",
            "default_namespace": "default",
            "supported_features": [NewOperatorFeature::PreviewEnv, NewOperatorFeature::PreviewSpecResources],
            "license": {"name": "test", "organization": "test", "expire_at": "2099-01-01"}
        }))
        .unwrap();
        assert!(require_spec_resources_support(&supported).is_ok());

        supported.operator_namespace = Some("mirrord".to_owned());
        assert!(matches!(
            require_spec_resources_support(&supported),
            Err(CliError::PreviewResourcesManagementOnly)
        ));
    }

    #[test]
    fn old_operator_rejects_tls_before_reading_credentials() {
        for config in [
            LocalTlsDelivery {
                client_cert: Some(PathBuf::from("/missing.pem")),
                client_key: Some(PathBuf::from("/missing.key")),
                ..Default::default()
            },
            LocalTlsDelivery {
                server_name: Some("app.example".to_owned()),
                ..Default::default()
            },
        ] {
            let mut values = BTreeMap::new();
            let error =
                resolve_tls_delivery(Some(&config), &mut values, &operator(false)).unwrap_err();
            assert!(
                matches!(error, CliError::FeatureNotSupportedInOperatorError { feature, .. } if feature == NewOperatorFeature::PreviewTlsDelivery.to_string())
            );
            assert!(values.is_empty());
        }
    }

    #[test]
    fn old_operator_accepts_preview_without_tls_overrides() {
        for config in [
            None,
            Some(LocalTlsDelivery::default()),
            Some(LocalTlsDelivery {
                protocol: TlsDeliveryProtocol::Tcp,
                ..Default::default()
            }),
        ] {
            let mut values = BTreeMap::new();
            let (delivery, _) =
                resolve_tls_delivery(config.as_ref(), &mut values, &operator(false)).unwrap();
            assert!(delivery.is_none());
            assert!(values.is_empty());
        }
    }

    #[test]
    fn preview_rejects_incomplete_credentials_even_with_tcp() {
        for protocol in [TlsDeliveryProtocol::Tcp, TlsDeliveryProtocol::Tls] {
            for cert_only in [true, false] {
                let config = LocalTlsDelivery {
                    protocol: protocol.clone(),
                    client_cert: cert_only.then(|| PathBuf::from("/missing.pem")),
                    client_key: (!cert_only).then(|| PathBuf::from("/missing.key")),
                    ..Default::default()
                };
                let mut values = BTreeMap::new();
                assert!(matches!(
                    resolve_tls_delivery(Some(&config), &mut values, &operator(true)),
                    Err(CliError::ConfigError(ConfigError::Conflict(_)))
                ));
                assert!(values.is_empty());
            }
        }
    }

    /// `protocol: tcp` makes exec ignore TLS settings, but a preview still delivers over TLS.
    /// `client_cert_source: target` with no paths must be rejected here, otherwise the preview
    /// starts and the pod rejects every stolen request for lack of a client certificate.
    #[test]
    fn preview_rejects_target_source_without_paths_even_with_tcp() {
        for protocol in [TlsDeliveryProtocol::Tcp, TlsDeliveryProtocol::Tls] {
            let config = LocalTlsDelivery {
                protocol,
                client_cert_source: TlsClientCertSource::Target,
                ..Default::default()
            };
            let mut values = BTreeMap::new();
            let error = resolve_tls_delivery(Some(&config), &mut values, &operator(true))
                .expect_err("target source without files must be rejected");
            assert!(
                matches!(error, CliError::ConfigError(ConfigError::Conflict(ref message)) if message.contains("client_cert_source")),
                "{error}",
            );
            assert!(values.is_empty());
        }
    }

    /// A preview delivers stolen TLS requests from the operator, which has no access to the
    /// user's files. The client certificate configured in `tls_delivery` must therefore be
    /// read by the CLI into the session's Secret, with the CR naming only the keys.
    #[test]
    fn client_cert_files_go_to_the_secret_and_keys_to_the_cr() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("client.pem");
        let key = dir.path().join("client.key");
        std::fs::write(&cert, b"CERT PEM").unwrap();
        std::fs::write(&key, b"KEY PEM").unwrap();

        let config = LocalTlsDelivery {
            client_cert: Some(cert),
            client_key: Some(key),
            server_name: Some("app.example".to_owned()),
            ..Default::default()
        };
        let mut secret_values = BTreeMap::new();
        let (tls_delivery, warnings) =
            resolve_tls_delivery(Some(&config), &mut secret_values, &operator(true)).unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            tls_delivery,
            Some(PreviewTlsDelivery {
                server_name: Some("app.example".to_owned()),
                client_auth: Some(PreviewTlsClientAuth {
                    cert_secret_key: "tls-client-cert".to_owned(),
                    key_secret_key: "tls-client-key".to_owned(),
                }),
                client_auth_from_target: None,
            })
        );
        assert_eq!(
            secret_values.get("tls-client-cert"),
            Some(&ByteString(b"CERT PEM".to_vec()))
        );
        assert_eq!(
            secret_values.get("tls-client-key"),
            Some(&ByteString(b"KEY PEM".to_vec()))
        );
    }

    /// With `client_cert_source: target` the paths name files in the target's container, so
    /// the CLI must not try to read them (they do not exist locally) and nothing goes into the
    /// Secret; the CR carries the paths for the operator.
    #[test]
    fn target_source_puts_paths_on_the_cr_and_reads_nothing() {
        let config = LocalTlsDelivery {
            client_cert_source: TlsClientCertSource::Target,
            client_cert: Some(PathBuf::from("/etc/tls/client.crt")),
            client_key: Some(PathBuf::from("/etc/tls/client.key")),
            ..Default::default()
        };
        let mut secret_values = BTreeMap::new();
        let (tls_delivery, warnings) = resolve_tls_delivery(
            Some(&config),
            &mut secret_values,
            &operator_with(&[
                NewOperatorFeature::PreviewTlsDelivery,
                NewOperatorFeature::PreviewTlsClientAuthFromTarget,
            ]),
        )
        .unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            tls_delivery,
            Some(PreviewTlsDelivery {
                server_name: None,
                client_auth: None,
                client_auth_from_target: Some(PreviewTlsClientAuthFromTarget {
                    cert_path: "/etc/tls/client.crt".to_owned(),
                    key_path: "/etc/tls/client.key".to_owned(),
                }),
            })
        );
        assert!(secret_values.is_empty());
    }

    /// An operator without the feature prunes `clientAuthFromTarget` from the CR and the
    /// preview pod rejects every stolen request with a TLS alert, so the CLI refuses up front.
    #[test]
    fn operator_without_target_source_support_is_rejected() {
        let config = LocalTlsDelivery {
            client_cert_source: TlsClientCertSource::Target,
            client_cert: Some(PathBuf::from("/etc/tls/client.crt")),
            client_key: Some(PathBuf::from("/etc/tls/client.key")),
            ..Default::default()
        };
        let mut secret_values = BTreeMap::new();
        let error =
            resolve_tls_delivery(Some(&config), &mut secret_values, &operator(true)).unwrap_err();

        assert!(
            matches!(error, CliError::FeatureNotSupportedInOperatorError { feature, .. } if feature == NewOperatorFeature::PreviewTlsClientAuthFromTarget.to_string())
        );
        assert!(secret_values.is_empty());
    }

    /// Settings a preview cannot honor are reported instead of silently ignored, and a config
    /// with nothing to honor leaves the CR as older CLIs send it.
    #[test]
    fn unsupported_settings_warn_and_produce_no_delivery_block() {
        let config = LocalTlsDelivery {
            protocol: TlsDeliveryProtocol::Tcp,
            trust_roots: Some(vec![PathBuf::from("/roots")]),
            ..Default::default()
        };
        let mut secret_values = BTreeMap::new();
        let (tls_delivery, warnings) =
            resolve_tls_delivery(Some(&config), &mut secret_values, &operator(true)).unwrap();

        assert_eq!(tls_delivery, None);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(secret_values.is_empty());
    }

    #[test]
    fn missing_client_cert_file_is_reported_with_its_path() {
        let config = LocalTlsDelivery {
            client_cert: Some(PathBuf::from("/definitely/missing.pem")),
            client_key: Some(PathBuf::from("/definitely/missing.key")),
            ..Default::default()
        };
        let mut secret_values = BTreeMap::new();
        let error =
            resolve_tls_delivery(Some(&config), &mut secret_values, &operator(true)).unwrap_err();

        assert!(
            matches!(&error, CliError::PreviewTlsClientAuthFile { path, .. } if path == Path::new("/definitely/missing.pem")),
            "{error}",
        );
    }
}

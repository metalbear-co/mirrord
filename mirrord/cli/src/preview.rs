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
    ops::Not,
    time::{Duration, Instant},
};

use futures::StreamExt;
use k8s_openapi::jiff::Timestamp;
use kube::{
    Api, ResourceExt,
    api::{DeleteParams, ListParams, ObjectMeta, PostParams},
    runtime::{
        wait::delete,
        watcher::{self, Event, watcher},
    },
};
use mirrord_analytics::{
    AnalyticsReporter, ExecutionKind,
    preview::{PreviewEvent, PreviewEventKind},
};
use mirrord_config::{
    LayerConfig,
    config::ConfigContext,
    feature::preview::PreviewTtlMins,
    target::{Target, TargetDisplay},
};
use mirrord_kube::api::runtime::RuntimeDataProvider;
use mirrord_operator::{
    client::{NoClientCert, OperatorApi},
    crd::{
        NewOperatorFeature, TARGET_NAMESPACE_ANNOTATION, TargetCrd,
        preview::{
            PreviewDbBranchingConfig, PreviewEnvVarsConfig, PreviewIncomingConfig,
            PreviewQueueSplittingConfig, PreviewSession, PreviewSessionPhase, PreviewSessionSpec,
        },
        session::SessionTarget,
    },
    types::OPERATOR_OWNERSHIP_LABEL,
};
use mirrord_progress::{Progress, ProgressTracker};
use tracing::Level;
use uuid::Uuid;

use crate::{
    config::{PreviewArgs, PreviewCommand, PreviewStartArgs, PreviewStatusArgs, PreviewStopArgs},
    error::{CliError, CliResult},
    user_data::UserData,
};

/// Handle commands related to preview environments: `mirrord preview ...`
pub(crate) async fn preview_command(
    args: PreviewArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    match args.command {
        PreviewCommand::Start(start_args) => preview_start(start_args, watch, user_data).await,
        PreviewCommand::Status(status_args) => preview_status(status_args, watch, user_data).await,
        PreviewCommand::Stop(stop_args) => preview_stop(stop_args, watch, user_data).await,
    }
}

/// Label key used to store the preview session's environment key on the CR.
///
/// This allows server-side filtering with `ListParams::labels()` in the `status` and `stop`
/// commands, avoiding the need to fetch all sessions and filter client-side.
pub const PREVIEW_SESSION_KEY_LABEL: &str = "preview.mirrord.metalbear.co/key";

/// Handle `mirrord preview start` command.
///
/// Creates a new preview environment or updates an existing one by creating
/// a `PreviewSession` resource that the operator will reconcile, then watches
/// the status until `Ready` or failure.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_start(
    args: PreviewStartArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview start");

    let layer_config = load_preview_config(args.as_env_vars(), &mut progress)?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch,
        user_data.machine_id(),
    );

    let (operator_api, api) =
        create_preview_api(&layer_config, false, &progress, &mut analytics).await?;
    operator_api.check_feature_support(&layer_config)?;

    // Create the `PreviewSession` resource in the cluster. The CR name is derived from
    // the target with a short random suffix to avoid collisions (e.g.
    // `preview-session-deploy-my-app-a1b2c3d4`). The operator watches for these resources
    // and reconciles them into preview pods.

    let mut subtask = progress.subtask("creating preview session resource");

    let config_target = layer_config.target.path.as_ref().ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewTargetRequired
    })?;

    let image = layer_config.feature.preview.image.as_ref().ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewImageRequired
    })?;

    let session_target = resolve_config_target(
        config_target,
        operator_api.client(),
        layer_config.target.namespace.as_deref(),
    )
    .await
    .inspect_err(|_| subtask.failure(None))?;

    // Check for an existing session with the same key+target.
    let key = layer_config.key.as_str();
    let list_params = ListParams::default().labels(&format!("{PREVIEW_SESSION_KEY_LABEL}={key}"));
    let existing_sessions = api.list(&list_params).await.map_err(|e| {
        subtask.failure(None);
        CliError::PreviewListFailed(e.to_string())
    })?;

    // Check if there's an existing session with the same key and warn the user about it.
    if existing_sessions.items.is_empty().not() {
        progress.warning(&format!(
            "the key '{key}' is already part of an existing preview environment. \
            If that’s not what you intended, please switch to a different key."
        ));
    }

    for session in existing_sessions
        .items
        .into_iter()
        .filter(|session| session.spec.target == session_target)
    {
        if !args.force {
            subtask.failure(None);
            return Err(CliError::PreviewDuplicateSession {
                key: key.to_owned(),
                target: config_target.to_string(),
            });
        }

        let name = session.name_any();

        subtask.warning(&format!("replacing existing session '{name}' (--force)",));

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

    let session_name = {
        let sanitized_target = config_target.to_string().replace('/', "-");
        let uuid_short = &Uuid::new_v4().simple().to_string()[..8];
        format!("preview-session-{sanitized_target}-{uuid_short}")
    };

    // Operators compiled with a custom OPERATOR_ISOLATION_MARKER only reconcile preview
    // sessions labeled with their marker (see the label selector in the preview-env
    // controller). The runtime env var check here allows developers to label the session
    // so it gets picked up by an isolated operator instead of the production one.
    let session_labels = {
        let mut labels = BTreeMap::from([(
            PREVIEW_SESSION_KEY_LABEL.to_owned(),
            layer_config.key.as_str().to_owned(),
        )]);
        if let Ok(marker) = std::env::var("OPERATOR_ISOLATION_MARKER") {
            labels.insert(OPERATOR_OWNERSHIP_LABEL.to_owned(), marker);
        }
        labels
    };

    let branch_db_names = operator_api
        .prepare_branch_dbs(&layer_config, &progress)
        .await?;

    let session_spec = PreviewSessionSpec {
        image: image.clone(),
        key: layer_config.key.as_str().to_owned(),
        target: session_target,
        ttl_secs: match layer_config.feature.preview.ttl_mins {
            PreviewTtlMins::Finite(mins) => mins.saturating_mul(60),
            PreviewTtlMins::Infinite(_) => PreviewTtlMins::INFINITE_TTL_SECS,
        },
        incoming: PreviewIncomingConfig::from_config(
            &layer_config.feature.network.incoming,
            layer_config.key.as_str(),
        ),
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
            BTreeMap::from([(
                TARGET_NAMESPACE_ANNOTATION.to_string(),
                target_ns.to_owned(),
            )])
        });

    let session = PreviewSession {
        metadata: ObjectMeta {
            name: Some(session_name),
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

    let pod_name = loop {
        tokio::select! {
            _ = &mut timeout => {
                if let Err(err) = delete::delete_and_finalize(api, &session.name_any(), &DeleteParams::default()).await {
                    subtask.warning(&format!(
                        "failed to delete timed out session '{}': {err}, \
                         you may need to delete it manually or with `mirrord preview stop`",
                        session.name_any(),
                    ));
                }

                subtask.failure(None);
                return Err(CliError::PreviewTimeout);
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
                                    subtask.success(Some("preview pod is ready"));
                                    break status.pod_name.clone().expect("Ready session must have pod_name");
                                }
                                PreviewSessionPhase::Failed => {
                                    let failure_message = status.failure_message.clone().expect("Failed session must have failure_message");
                                    subtask.failure(None);
                                    return Err(CliError::PreviewSessionFailed(failure_message));
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
    };

    // Display summary of the created preview environment.

    let namespace = layer_config
        .target
        .namespace
        .as_deref()
        .unwrap_or(operator_api.client().default_namespace());

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
    progress.subtask(&format!("pod: {pod_name}")).success(None);

    Ok(())
}

/// Handle `mirrord preview status` command.
///
/// Lists preview environments, optionally filtered by key, namespace, and whether failed
/// sessions should be shown.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_status(
    args: PreviewStatusArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview status");

    let layer_config = load_preview_config(args.as_env_vars(), &mut progress)?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch.clone(),
        user_data.machine_id(),
    );

    // Default to all namespaces when no namespace is configured, so `mirrord preview status`
    // with no flags shows everything.
    let all_namespaces = args.all_namespaces || layer_config.target.namespace.is_none();
    let (_, api) =
        create_preview_api(&layer_config, all_namespaces, &progress, &mut analytics).await?;

    // List and filter sessions.

    let mut subtask = progress.subtask("listing preview sessions");

    let key_filter = layer_config.key.provided();
    let list_params = match key_filter {
        Some(key) => ListParams::default().labels(&format!("{PREVIEW_SESSION_KEY_LABEL}={key}")),
        None => ListParams::default(),
    };

    let sessions: Vec<_> = api
        .list(&list_params)
        .await
        .map_err(|e| {
            subtask.failure(None);
            CliError::PreviewListFailed(e.to_string())
        })?
        .items;

    let sessions: Vec<_> = sessions
        .iter()
        .filter(|session| {
            let (failed, expired) = session
                .status
                .as_ref()
                .map(|status| {
                    let failed_phase = matches!(status.phase, PreviewSessionPhase::Failed);

                    // Older operators reused the `Failed` phase with a specific failure message
                    // when the preview TTL elapsed instead of deleting them, so we need to handle
                    // that to hide expired preview sessions in a backwards compatible way.
                    // See https://github.com/metalbear-co/operator/blob/17e4c645d59affefc672f597a10e2880c405f043/crates/operator-preview-env/src/task.rs#L774-L775
                    let expired = failed_phase
                        && status.failure_message.as_deref() == Some("preview session TTL expired");

                    let failed = failed_phase && !expired;

                    (failed, expired)
                })
                .unwrap_or_default();

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

    // Display sessions grouped by key.

    let mut grouped: BTreeMap<&str, Vec<&PreviewSession>> = BTreeMap::new();

    for session in &sessions {
        grouped
            .entry(session.spec.key.as_str())
            .or_default()
            .push(session);
    }

    for (key, sessions) in grouped {
        println!("  {key}:",);

        for session in sessions.iter() {
            let pod_name = session
                .status
                .as_ref()
                .and_then(|s| s.pod_name.as_deref())
                .unwrap_or("<unknown>");

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
                Some(PreviewSessionPhase::Unknown) => "unknown".to_owned(),
                None => "pending".to_owned(),
            };

            println!(
                "    * {} ({} @ {}): {}",
                pod_name,
                session.spec.target,
                session.metadata.namespace.as_deref().unwrap_or("<unknown>"),
                status
            );

            PreviewEvent::new(
                &session.spec.key,
                session.runtime_secs(),
                PreviewEventKind::Status,
            )
            .report_analytics(
                layer_config.telemetry,
                watch.clone(),
                user_data.machine_id(),
            );
        }
    }

    Ok(())
}
/// Handle `mirrord preview stop` command.
///
/// Deletes preview environments matching the given key and, optionally, a target filter and
/// namespace.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_stop(
    args: PreviewStopArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview stop");

    let layer_config = load_preview_config(args.as_env_vars(), &mut progress)?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch,
        user_data.machine_id(),
    );

    let key = layer_config
        .key
        .provided()
        .ok_or(CliError::PreviewKeyRequired)?
        .to_owned();

    // Default to all namespaces when no namespace is configured, same as `status`.
    let all_namespaces = args.all_namespaces || layer_config.target.namespace.is_none();
    let (operator_api, api) =
        create_preview_api(&layer_config, all_namespaces, &progress, &mut analytics).await?;

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

    let list_params = ListParams::default().labels(&format!("{PREVIEW_SESSION_KEY_LABEL}={key}"));

    let sessions_to_delete: Vec<_> = api
        .list(&list_params)
        .await
        .map_err(|e| {
            subtask.failure(None);
            CliError::PreviewListFailed(e.to_string())
        })?
        .items
        .into_iter()
        .filter(|session| {
            session_target
                .as_ref()
                .is_none_or(|target| session.spec.target == *target)
        })
        .collect();

    if sessions_to_delete.is_empty() {
        subtask.failure(None);
        return Err(CliError::PreviewNotFound(key));
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

/// Resolves a [`Target`] to a [`SessionTarget`] by fetching the target from the
/// operator's GET TargetCrd API. The operator validates the target exists and resolves
/// the container if not specified. Works for both single-cluster and multi-cluster.
///
/// Falls back to local `runtime_data` resolution if the operator didn't resolve the
/// container (backwards compatibility with older operators).
async fn resolve_config_target(
    config_target: &Target,
    client: &kube::Client,
    namespace: Option<&str>,
) -> CliResult<SessionTarget> {
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

fn load_preview_config(
    env_overrides: HashMap<&OsStr, Cow<'_, OsStr>>,
    progress: &mut ProgressTracker,
) -> CliResult<LayerConfig> {
    let mut subtask = progress.subtask("loading configuration");

    let mut cfg_context = ConfigContext::default().override_envs(env_overrides);

    let config = LayerConfig::resolve(&mut cfg_context).inspect_err(|_| {
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

/// Connects to the operator, validates the license and checks that the `PreviewEnv` feature is
/// supported, then returns the operator API and a `PreviewSession` API handle scoped to the
/// appropriate namespace(s).
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
            CliError::OperatorNotInstalled
        })?;

    operator_api.check_license_validity(progress)?;

    operator_api
        .operator()
        .spec
        .require_feature(NewOperatorFeature::PreviewEnv)
        .inspect_err(|_| {
            subtask.failure(None);
        })?;

    subtask.success(Some("connected to operator"));

    let client = operator_api.client().clone();
    let operator_namespace = operator_api.operator().spec.operator_namespace.as_deref();
    let target_namespace = config.target.namespace.as_deref();

    let api = if all_namespaces {
        Api::all(client)
    } else if let Some(namespace) = operator_namespace.or(target_namespace) {
        Api::namespaced(client, namespace)
    } else {
        Api::default_namespaced(client)
    };

    Ok((operator_api, api))
}

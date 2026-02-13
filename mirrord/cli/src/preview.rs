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
    time::{Duration, Instant},
};

use futures::StreamExt;
use kube::{
    Api, ResourceExt,
    api::{DeleteParams, ListParams, ObjectMeta, PostParams},
    runtime::watcher::{self, Event, watcher},
};
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext, target::TargetDisplay};
use mirrord_kube::api::runtime::RuntimeDataProvider;
use mirrord_operator::{
    client::OperatorApi,
    crd::{
        NewOperatorFeature,
        preview::{PreviewIncomingConfig, PreviewSession, PreviewSessionPhase, PreviewSessionSpec},
        session::SessionTarget,
    },
};
use mirrord_progress::{Progress, ProgressTracker};
use tracing::Level;
use uuid::Uuid;

use crate::{
    config::{PreviewArgs, PreviewCommand, PreviewStartArgs, PreviewStatusArgs, PreviewStopArgs},
    error::{CliError, CliResult},
};

/// Handle commands related to preview environments: `mirrord preview ...`
pub(crate) async fn preview_command(args: PreviewArgs) -> CliResult<()> {
    match args.command {
        PreviewCommand::Start(start_args) => preview_start(start_args).await,
        PreviewCommand::Status(status_args) => preview_status(status_args).await,
        PreviewCommand::Stop(stop_args) => preview_stop(stop_args).await,
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
async fn preview_start(args: PreviewStartArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview start");

    let layer_config = load_preview_config(args.as_env_vars(), &mut progress)?;

    let (client, api) = create_preview_api(&layer_config, false, &progress).await?;

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

    let mut target = config_target.clone();
    if target.container().is_none() {
        let runtime_data = target
            .runtime_data(&client, layer_config.target.namespace.as_deref())
            .await
            .map_err(CliError::RuntimeDataResolution)?;

        if runtime_data.guessed_container {
            subtask.warning(&format!(
                "Target has multiple containers, mirrord picked \"{}\". \
                 To target a different one, include it in the target path.",
                runtime_data.container_name
            ));
        }

        target.set_container(runtime_data.container_name);
    }

    let target = SessionTarget::from_config(target).ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewTargetResolutionFailed(config_target.to_string())
    })?;

    let spec = PreviewSessionSpec {
        image: image.clone(),
        key: layer_config.key.as_str().to_owned(),
        target,
        incoming: PreviewIncomingConfig::from_config(&layer_config.feature.network.incoming),
        ttl_secs: layer_config.feature.preview.ttl_mins * 60,
    };

    // Check for an existing session with the same key+target.
    let key = layer_config.key.as_str();
    let list_params = ListParams::default().labels(&format!("{PREVIEW_SESSION_KEY_LABEL}={key}"));
    let existing = api.list(&list_params).await.map_err(|e| {
        subtask.failure(None);
        CliError::PreviewListFailed(e.to_string())
    })?;

    for session in existing.items {
        if session.spec.target != spec.target {
            continue;
        }
        match session.status.as_ref().map(|s| &s.phase) {
            // Failed session — delete it so we can take its place.
            Some(PreviewSessionPhase::Failed) => {
                let name = session.name_any();
                if let Err(e) = api.delete(&name, &DeleteParams::default()).await {
                    subtask.warning(&format!(
                        "failed to delete failed session '{name}': {e}, \
                         you may need to delete it manually or with `mirrord preview stop`",
                    ));
                }
            }
            // Active session — reject the duplicate.
            _ => {
                subtask.failure(None);
                return Err(CliError::PreviewDuplicateSession {
                    key: key.to_owned(),
                    target: spec.target.to_string(),
                });
            }
        }
    }

    let sanitized_target = config_target.to_string().replace('/', "-");
    let uuid_short = Uuid::new_v4().simple().to_string();
    let uuid_short = &uuid_short[..8];
    let preview = PreviewSession {
        metadata: ObjectMeta {
            name: Some(format!("preview-session-{sanitized_target}-{uuid_short}")),
            labels: Some(BTreeMap::from([(
                PREVIEW_SESSION_KEY_LABEL.to_owned(),
                layer_config.key.as_str().to_owned(),
            )])),
            ..Default::default()
        },
        spec,
        status: None,
    };

    let session = api
        .create(&PostParams::default(), &preview)
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

    let stream = watcher(
        api.clone(),
        watcher::Config::default().fields(&format!("metadata.name={}", session.name_any())),
    );
    tokio::pin!(stream);

    let initialization_start = Instant::now();
    let mut long_initialization_timer = tokio::time::interval(Duration::from_secs(20));
    // First tick is instant
    long_initialization_timer.tick().await;

    let timeout = tokio::time::sleep(Duration::from_secs(
        layer_config.feature.preview.creation_timeout_secs,
    ));
    tokio::pin!(timeout);

    let mut last_known_phase: &str = "unknown";

    let pod_name = loop {
        tokio::select! {
            _ = &mut timeout => {
                if let Err(err) = api.delete(&session.name_any(), &DeleteParams::default()).await {
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
                                    // Sessions that fail to spawn should not be retained —
                                    // delete the CRD so the operator can clean up and the
                                    // user can retry without stale resources blocking them.
                                    if let Err(err) = api.delete(&session.name_any(), &DeleteParams::default()).await {
                                        subtask.warning(&format!(
                                            "failed to delete failed session '{}': {err}, \
                                             you may need to delete it manually or with `mirrord preview stop`",
                                            session.name_any(),
                                        ));
                                    }
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
        .unwrap_or(client.default_namespace());

    progress.success(None);

    let key = layer_config.key.as_str();

    println!(
        r#"
  info:
    * key: {key}
    * namespace: {namespace}
    * pod: {pod_name}"#
    );

    Ok(())
}

/// Handle `mirrord preview status` command.
///
/// Lists preview environments, optionally filtered by key and namespace.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_status(args: PreviewStatusArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview status");

    let layer_config = load_preview_config(args.as_env_vars(), &mut progress)?;

    // Default to all namespaces when no namespace is configured, so `mirrord preview status`
    // with no flags shows everything.
    let all_namespaces = args.all_namespaces || layer_config.target.namespace.is_none();
    let (_, api) = create_preview_api(&layer_config, all_namespaces, &progress).await?;

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

            let status = match session.status.as_ref().map(|s| &s.phase) {
                Some(PreviewSessionPhase::Initializing) => "initializing".to_owned(),
                Some(PreviewSessionPhase::Waiting) => "waiting".to_owned(),
                Some(PreviewSessionPhase::Ready) => "running".to_owned(),
                Some(PreviewSessionPhase::Failed) => {
                    let msg = session
                        .status
                        .as_ref()
                        .and_then(|s| s.failure_message.as_deref())
                        .unwrap_or("unknown");
                    format!("failed ({msg})")
                }
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
        }
    }

    Ok(())
}

/// Handle `mirrord preview stop` command.
///
/// Deletes preview environments matching the given key and, optionally, a target filter and
/// namespace.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_stop(args: PreviewStopArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview stop");

    let layer_config = load_preview_config(args.as_env_vars(), &mut progress)?;

    // env-key is required for stop command.
    let key = layer_config
        .key
        .provided()
        .ok_or(CliError::PreviewKeyRequired)?
        .to_owned();

    // Default to all namespaces when no namespace is configured, same as `status`.
    let all_namespaces = args.all_namespaces || layer_config.target.namespace.is_none();
    let (client, api) = create_preview_api(&layer_config, all_namespaces, &progress).await?;

    // Find sessions matching the key and optional target filter.

    let mut subtask = progress.subtask("finding preview sessions");

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
            args.target
                .as_ref()
                .is_none_or(|target| session.spec.target.to_string() == *target)
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

        let namespaced_api = Api::<PreviewSession>::namespaced(client.clone(), namespace);

        if let Err(e) = namespaced_api.delete(name, &DeleteParams::default()).await {
            delete_subtask.failure(None);
            return Err(CliError::PreviewDeleteFailed {
                name: name.to_owned(),
                reason: e.to_string(),
            });
        }
    }

    delete_subtask.success(Some("all sessions deleted"));
    progress.success(None);

    Ok(())
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

    subtask.success(Some("configuration loaded"));

    Ok(config)
}

/// Connects to the operator, validates the license and checks that the `PreviewEnv` feature is
/// supported, then returns a Kubernetes client and a `PreviewSession` API handle scoped to
/// the appropriate namespace(s).
async fn create_preview_api(
    config: &LayerConfig,
    all_namespaces: bool,
    progress: &ProgressTracker,
) -> CliResult<(kube::Client, Api<PreviewSession>)> {
    let mut subtask = progress.subtask("connecting to operator");

    let operator_api = OperatorApi::try_new(config, &mut NullReporter::default(), progress)
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
    let api = if all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = config.target.namespace.as_deref() {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    };

    Ok((client, api))
}

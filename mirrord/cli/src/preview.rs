//! Handlers for `mirrord preview` commands.
//!
//! The CLI is responsible for creating `PreviewSessionCrd` resources and watching their status —
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
    client::ClientBuilder,
    runtime::watcher::{self, Event, watcher},
};
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_kube::{api::kubernetes::create_kube_config, retry::RetryKube};
use mirrord_operator::{
    client::OperatorApi,
    crd::preview::{
        PreviewIncomingConfig, PreviewSession, PreviewSessionCrd, PreviewSessionStatus,
    },
};
use mirrord_progress::{Progress, ProgressTracker};
use tower::{buffer::BufferLayer, retry::RetryLayer};
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
/// a `PreviewSessionCrd` resource that the operator will reconcile, then watches
/// the status until `Ready` or failure.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_start(args: PreviewStartArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview start");

    let layer_config = load_preview_config(args.as_env_vars(), &mut progress)?;

    // Connect to the operator — preview environments require an operator deployment.

    let mut subtask = progress.subtask("connecting to operator");

    let operator_api = OperatorApi::try_new(&layer_config, &mut NullReporter::default(), &progress)
        .await?
        .ok_or_else(|| {
            subtask.failure(None);
            CliError::OperatorRequiredForPreview
        })?;

    operator_api.check_license_validity(&progress)?;

    subtask.success(Some("connected to operator"));

    // Create the `PreviewSessionCrd` resource in the cluster. The CR name is derived from
    // the target with a short random suffix to avoid collisions (e.g.
    // `preview-session-deploy-my-app-a1b2c3d4`). The operator watches for these resources
    // and reconciles them into preview pods.

    let mut subtask = progress.subtask("creating preview session resource");

    let namespace = layer_config
        .target
        .namespace
        .as_deref()
        .unwrap_or(operator_api.client().default_namespace());

    let target = layer_config.target.path.as_ref().ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewTargetRequired
    })?;

    let image = layer_config.feature.preview.image.as_ref().ok_or_else(|| {
        subtask.failure(None);
        CliError::PreviewImageRequired
    })?;

    let spec = PreviewSession {
        image: image.clone(),
        key: layer_config.key.as_str().to_owned(),
        target: target.to_string(),
        incoming: PreviewIncomingConfig::from(&layer_config.feature.network.incoming),
        ttl_mins: layer_config.feature.preview.ttl_mins,
    };

    let api = Api::<PreviewSessionCrd>::namespaced(operator_api.client().clone(), namespace);

    let sanitized_target = target.to_string().replace('/', "-");
    let uuid_short = Uuid::new_v4().simple().to_string();
    let uuid_short = &uuid_short[..8];
    let preview = PreviewSessionCrd {
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

    // Watch the `PreviewSessionCrd` status until it reaches `Ready` or `Failed`. Emits
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
                            match status {
                                PreviewSessionStatus::Initializing { .. } => {
                                    last_known_phase = "initializing preview env";
                                }
                                PreviewSessionStatus::Waiting { .. } => {
                                    last_known_phase = "waiting for preview pod to be ready";
                                }
                                PreviewSessionStatus::Ready { pod_name } => {
                                    subtask.success(Some("preview pod is ready"));
                                    break pod_name.clone();
                                }
                                PreviewSessionStatus::Failed { failure_message, .. } => {
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
                                    return Err(CliError::PreviewSessionFailed(failure_message.clone()));
                                }
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
    let (_, api) = create_client_and_api(&layer_config, all_namespaces).await?;

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

    let mut grouped: BTreeMap<&str, Vec<&PreviewSessionCrd>> = BTreeMap::new();

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
                .and_then(PreviewSessionStatus::preview_pod_name)
                .map(String::as_str)
                .unwrap_or("<unknown>");

            let status = match &session.status {
                Some(PreviewSessionStatus::Initializing { .. }) => "initializing".to_owned(),
                Some(PreviewSessionStatus::Waiting { .. }) => "waiting".to_owned(),
                Some(PreviewSessionStatus::Ready { .. }) => "running".to_owned(),
                Some(PreviewSessionStatus::Failed {
                    failure_message, ..
                }) => {
                    format!("failed ({failure_message})")
                }
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
    let (client, api) = create_client_and_api(&layer_config, all_namespaces).await?;

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

        let namespaced_api = Api::<PreviewSessionCrd>::namespaced(client.clone(), namespace);

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

/// Creates a Kubernetes client and a `PreviewSessionCrd` API handle scoped to the appropriate
/// namespace(s). When `all_namespaces` is true the API queries across all namespaces; otherwise
/// it uses the namespace from the config (or the default namespace).
async fn create_client_and_api(
    config: &LayerConfig,
    all_namespaces: bool,
) -> CliResult<(kube::Client, Api<PreviewSessionCrd>)> {
    let client = create_kube_config(
        config.accept_invalid_certificates,
        config.kubeconfig.clone(),
        config.kube_context.clone(),
    )
    .await
    .and_then(|kube_config| {
        Ok(ClientBuilder::try_from(kube_config)?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(RetryKube::try_from(
                &config.startup_retry,
            )?))
            .build())
    })
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;

    let api = if all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = config.target.namespace.as_deref() {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    };

    Ok((client, api))
}

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
use k8s_openapi::chrono::Utc;
use kube::{
    Api, ResourceExt,
    api::{DeleteParams, ListParams, ObjectMeta, PostParams},
    runtime::{
        wait::delete,
        watcher::{self, Event, watcher},
    },
};
use mirrord_analytics::NullReporter;
use mirrord_config::{
    LayerConfig,
    config::ConfigContext,
    target::{Target, TargetDisplay},
};
use mirrord_kube::api::runtime::RuntimeDataProvider;
use mirrord_operator::{
    client::OperatorApi,
    crd::{
        NewOperatorFeature,
        preview::{PreviewIncomingConfig, PreviewSession, PreviewSessionPhase, PreviewSessionSpec},
        session::SessionTarget,
    },
    types::OPERATOR_OWNERSHIP_LABEL,
};
use mirrord_progress::{Progress, ProgressTracker};
use oci_spec::distribution::Reference;
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

    let session_target = resolve_config_target(
        config_target,
        &client,
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

    for session in existing_sessions
        .items
        .into_iter()
        .filter(|session| session.spec.target == session_target)
    {
        let phase = session.status.as_ref().map(|s| s.phase);
        // Check if we're replacing a failed/stale session or updating the config/image of an
        // active session
        if matches!(phase, None | Some(PreviewSessionPhase::Failed))
            || images_match(&session.spec.image, image)?
        {
            let name = session.name_any();

            subtask.warning(&format!(
                "replacing existing session '{name}' with updated parameters",
            ));

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

        // Reaching this point means we're not replacing an existing session, so we're trying to
        // create a duplicate one.
        subtask.failure(None);
        return Err(CliError::PreviewDuplicateSession {
            key: key.to_owned(),
            target: config_target.to_string(),
        });
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

    let session_spec = PreviewSessionSpec {
        image: image.clone(),
        key: layer_config.key.as_str().to_owned(),
        target: session_target,
        incoming: PreviewIncomingConfig::from_config(&layer_config.feature.network.incoming),
        ttl_secs: layer_config.feature.preview.ttl_mins * 60,
    };

    let session = PreviewSession {
        metadata: ObjectMeta {
            name: Some(session_name),
            labels: Some(session_labels),
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
                                    // Sessions that fail to spawn should not be retained —
                                    // delete the CRD so the operator can clean up and the
                                    // user can retry without stale resources blocking them.
                                    if let Err(err) = delete::delete_and_finalize(api, &session.name_any(), &DeleteParams::default()).await {
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
                Some(PreviewSessionPhase::Ready) => {
                    let remaining = session
                        .status
                        .as_ref()
                        .and_then(|s| s.expires_at.as_ref())
                        .and_then(|expires_at| (expires_at.0 - Utc::now()).to_std().ok())
                        .map(|d| Duration::from_secs(d.as_secs()));
                    match remaining {
                        Some(d) => format!("running ({} remaining)", humantime::format_duration(d)),
                        None => "running".to_owned(),
                    }
                }
                Some(PreviewSessionPhase::Failed) => {
                    let msg = session
                        .status
                        .as_ref()
                        .and_then(|s| s.failure_message.as_deref())
                        .unwrap_or("unknown");
                    format!("stopped ({msg})")
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

    let key = layer_config
        .key
        .provided()
        .ok_or(CliError::PreviewKeyRequired)?
        .to_owned();

    // Default to all namespaces when no namespace is configured, same as `status`.
    let all_namespaces = args.all_namespaces || layer_config.target.namespace.is_none();
    let (client, api) = create_preview_api(&layer_config, all_namespaces, &progress).await?;

    let mut subtask = progress.subtask("finding preview sessions");

    // Resolve the config target (if provided) to a full SessionTarget for comparison.
    // This allows the user to type, for example, "deployment/foo" and have it match a session that
    // has `spec.target` set to "Deployment/foo/container/foo"
    let session_target = match &layer_config.target.path {
        Some(config_target) => {
            let session_target = resolve_config_target(
                config_target,
                &client,
                layer_config.target.namespace.as_deref(),
            )
            .await
            .inspect_err(|_| subtask.failure(None))?;

            Some(session_target)
        }
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

        let namespaced_api = Api::<PreviewSession>::namespaced(client.clone(), namespace);

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

/// Resolves a [`Target`] to a [`SessionTarget`] by auto-detecting the container if not
/// specified, then converting to the session representation.
async fn resolve_config_target(
    config_target: &Target,
    client: &kube::Client,
    namespace: Option<&str>,
) -> CliResult<SessionTarget> {
    let mut config_target_with_container = config_target.clone();
    if config_target_with_container.container().is_none() {
        let runtime_data = config_target
            .runtime_data(client, namespace)
            .await
            .map_err(CliError::RuntimeDataResolution)?;
        config_target_with_container.set_container(runtime_data.container_name);
    }

    SessionTarget::from_config(config_target_with_container)
        .ok_or_else(|| CliError::PreviewTargetResolutionFailed(config_target.to_string()))
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

/// Returns `true` when two OCI image references point at the same repository, ignoring any tag or
/// digest difference. Used to determine whether we should redeploy/update an existing preview env.
fn images_match(a: &str, b: &str) -> CliResult<bool> {
    let a: Reference = a
        .parse()
        .map_err(|e| CliError::PreviewInvalidImage(format!("{a}: {e}")))?;
    let b: Reference = b
        .parse()
        .map_err(|e| CliError::PreviewInvalidImage(format!("{b}: {e}")))?;
    Ok(a.registry() == b.registry() && a.repository() == b.repository())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::images_match;

    #[rstest]
    #[case("myapp:v1", "myapp:v2", true)]
    #[case("myapp:v1", "myapp:v1", true)]
    #[case("myapp", "myapp:latest", true)]
    #[case("ghcr.io/org/app:v1", "ghcr.io/org/app:v2", true)]
    #[case("registry:5000/app:v1", "registry:5000/app:v2", true)]
    #[case("myapp:v1", "otherapp:v1", false)]
    #[case("ghcr.io/org/app:v1", "docker.io/org/app:v1", false)]
    fn test_images_same_name(#[case] a: &str, #[case] b: &str, #[case] expected: bool) {
        assert_eq!(images_match(a, b).unwrap(), expected);
    }
}

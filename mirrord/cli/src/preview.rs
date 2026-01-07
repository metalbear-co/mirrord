//! Handlers for `mirrord preview` commands.
//!
//! Preview environments allow developers to deploy their code to the cluster
//! in a persistent pod that can be shared with others for testing and review.

use std::time::{Duration, Instant};

use drain::Watch;
use futures::StreamExt;
use kube::{
    Api, ResourceExt,
    api::PostParams,
    runtime::watcher::{self, Event, watcher},
};
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::{
    client::{NoClientCert, OperatorApi},
    crd::preview::{
        PreviewIncomingConfig, PreviewSession, PreviewSessionCrd, PreviewSessionStatus,
    },
};
use mirrord_progress::{Progress, ProgressTracker};
use tokio::{pin, time::interval};
use tracing::Level;

use crate::{
    config::{PreviewArgs, PreviewCommand, PreviewStartArgs},
    error::{CliError, CliResult},
    user_data::UserData,
};

/// Handle commands related to preview environments: `mirrord preview ...`
pub(crate) async fn preview_command(
    args: PreviewArgs,
    _watch: Watch,
    _user_data: &UserData,
) -> CliResult<()> {
    match args.command {
        PreviewCommand::Start(start_args) => preview_start(start_args).await,
    }
}

/// Handle `mirrord preview start` command.
///
/// Creates a new preview environment or updates an existing one by creating
/// a `PreviewSessionCrd` resource that the operator will reconcile.
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn preview_start(args: PreviewStartArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord preview start");

    let layer_config = load_preview_config(&args, &mut progress)?;

    let operator_api = connect_to_operator(&layer_config, &mut progress).await?;

    let session = create_session(&operator_api, &args, &layer_config, &mut progress).await?;

    let session = wait_for_status_ready(&operator_api, &session, &mut progress).await?;

    display_preview_info(&session, layer_config.key.as_str(), &mut progress);

    Ok(())
}

/// Load and resolve the mirrord configuration for preview.
fn load_preview_config(
    args: &PreviewStartArgs,
    progress: &mut ProgressTracker,
) -> CliResult<LayerConfig> {
    let mut subtask = progress.subtask("loading configuration");

    let mut cfg_context = ConfigContext::default().override_envs(args.as_env_vars());

    let config = LayerConfig::resolve(&mut cfg_context).inspect_err(|error| {
        subtask.failure(Some(&format!("failed to read config: {error}")));
    })?;

    subtask.success(Some("configuration loaded"));
    Ok(config)
}

/// Connect to the mirrord operator.
///
/// The operator is required for preview environments.
async fn connect_to_operator(
    config: &LayerConfig,
    progress: &mut ProgressTracker,
) -> CliResult<OperatorApi<NoClientCert>> {
    let mut subtask = progress.subtask("connecting to operator");

    let operator_api = OperatorApi::try_new(config, &mut NullReporter::default(), progress)
        .await?
        .ok_or_else(|| {
            subtask.failure(Some("operator not found"));
            CliError::OperatorRequiredForPreview
        })?;

    operator_api.check_license_validity(progress)?;

    subtask.success(Some("connected to operator"));
    Ok(operator_api)
}

/// Create the PreviewSessionCrd resource in the cluster.
///
/// Returns the created CRD and the namespace it was created in.
async fn create_session(
    operator_api: &OperatorApi<NoClientCert>,
    args: &PreviewStartArgs,
    config: &LayerConfig,
    progress: &mut ProgressTracker,
) -> CliResult<PreviewSessionCrd> {
    let mut subtask = progress.subtask("creating preview environment");

    let namespace = config
        .target
        .namespace
        .as_deref()
        .unwrap_or(operator_api.client().default_namespace());

    let target = config.target.path.as_ref().ok_or_else(|| {
        subtask.failure(Some("target is required for preview environments"));
        CliError::PreviewCreationFailed("target is required for preview environments".to_owned())
    })?;

    let spec = PreviewSession {
        image: args.image.clone(),
        key: config.key.as_str().to_owned(),
        target: target.to_string(),
        target_namespace: config.target.namespace.clone(),
        incoming: PreviewIncomingConfig::from(&config.feature.network.incoming),
    };

    let preview = PreviewSessionCrd::new(&format!("preview-session-{}", config.key.as_str()), spec);

    let api = Api::<PreviewSessionCrd>::namespaced(operator_api.client().clone(), namespace);

    let session = api
        .create(&PostParams::default(), &preview)
        .await
        .map_err(|e| {
            subtask.failure(Some(&format!("failed to create preview: {e}")));
            CliError::PreviewCreationFailed(e.to_string())
        })?;

    subtask.success(Some("preview environment created"));

    Ok(session)
}

/// Wait for the preview to become ready using a watcher.
///
/// Returns the updated preview CRD with status populated.
async fn wait_for_status_ready(
    operator_api: &OperatorApi<NoClientCert>,
    session: &PreviewSessionCrd,
    progress: &mut ProgressTracker,
) -> CliResult<PreviewSessionCrd> {
    let mut subtask = progress.subtask("waiting for preview to be ready");

    let namespace = session.metadata.namespace.as_deref().unwrap_or_default();

    let api: Api<PreviewSessionCrd> = Api::namespaced(operator_api.client().clone(), namespace);

    let stream = watcher(
        api,
        watcher::Config::default().fields(&format!("metadata.name={}", session.name_any())),
    );
    pin!(stream);

    let initialization_start = Instant::now();
    let mut long_initialization_timer = interval(Duration::from_secs(20));
    // First tick is instant
    long_initialization_timer.tick().await;

    let mut last_known_phase: &str = "unknown";

    loop {
        tokio::select! {
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
                                PreviewSessionStatus::Initializing { ..} => {
                                    last_known_phase = "initializing preview env";
                                }
                                PreviewSessionStatus::Waiting {..} => {
                                    last_known_phase = "waiting for preview pod to be ready";
                                }
                                PreviewSessionStatus::Ready {..} => {
                                    subtask.success(Some("preview is ready"));
                                    return Ok(current);
                                }
                                PreviewSessionStatus::Failed { failure_message, .. } => {
                                    subtask.failure(Some(&format!("preview creation failed: {failure_message}")));
                                    return Err(CliError::PreviewCreationFailed(failure_message.clone()));
                                }
                            }
                        }
                    }

                    Some(Ok(Event::Delete(_))) => {
                        subtask.failure(Some("preview env was unexpectedly deleted"));
                        return Err(CliError::PreviewCreationFailed(
                            "preview was unexpectedly deleted".to_string(),
                        ));
                    }

                    Some(Ok(Event::Init | Event::InitDone)) => continue,

                    Some(Err(error)) => {
                        subtask.failure(Some("watch stream failed"));
                        return Err(CliError::PreviewCreationFailed(format!(
                            "watch stream failed: {error}"
                        )));
                    }

                    None => {
                        subtask.failure(Some("preview creation timed out"));
                        return Err(CliError::PreviewTimeout);
                    }
                }
            }
        }
    }
}

/// Display information about the created preview environment.
fn display_preview_info(preview: &PreviewSessionCrd, key: &str, progress: &mut ProgressTracker) {
    let namespace = preview.metadata.namespace.as_deref().unwrap_or("<unknown>");
    let session = preview.metadata.name.as_deref().unwrap_or("<unknown>");

    let Some(PreviewSessionStatus::Ready { pod_name }) = &preview.status else {
        unreachable!("malformed status");
    };

    progress.success(Some(&format!(
        r#"
preview environment created successfully

- environment key: {key}
- namespace: {namespace}
- session: {session}
- pod: {pod_name}
"#
    )));
}

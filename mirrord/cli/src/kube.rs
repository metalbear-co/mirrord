use std::{
    fmt::Debug,
    sync::{Mutex, MutexGuard},
};

use kube::{Resource, api::ListParams, client::ClientBuilder};
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::kubernetes::{KubeEnvironment, create_kube_config, resolve_kube_environment},
    error::KubeApiError,
    retry::retry_policy_from_config,
};
use mirrord_operator::client::add_baggage_header;
use mirrord_progress::Progress;
use serde::de::DeserializeOwned;
use tower::{buffer::BufferLayer, retry::RetryLayer};

use crate::error::CliError;

/// Create a kube client according to the layer config, carrying its `baggage`, with a request
/// buffer of 1024 requests and a retry policy according to the layer config.
pub(crate) async fn kube_client_from_layer_config(
    layer_config: &LayerConfig,
) -> Result<kube::Client, CliError> {
    let mut config = create_kube_config(
        layer_config.accept_invalid_certificates,
        layer_config.kubeconfig.clone(),
        layer_config.kube_context.clone(),
    )
    .await
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;
    add_baggage_header(&mut config, layer_config.baggage.as_deref())?;

    ClientBuilder::try_from(config)
        .map_err(KubeApiError::from)
        .and_then(|builder| {
            Ok(builder
                .with_layer(&BufferLayer::new(1024))
                .with_layer(&RetryLayer::new(retry_policy_from_config(
                    &layer_config.startup_retry,
                )?))
                .build())
        })
        .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))
}

/// Get a vector of `T`s if T is defined on the cluster. If the list request returns a 404, assume
/// `T` is not defined on this cluster and return `Ok(None)`. Any other error gets converted to a
/// `CliError`.
pub(crate) async fn list_resource_if_defined<R, P>(
    resource_api: &kube::Api<R>,
    status_progress: &mut P,
) -> Result<Option<Vec<R>>, CliError>
where
    R: Resource<DynamicType = ()> + Clone + Debug + DeserializeOwned,
    P: Progress,
{
    match resource_api.list(&ListParams::default()).await {
        Ok(branches) => Ok(Some(branches.items)),
        Err(kube::Error::Api(err)) if err.code == 404 => {
            status_progress.info(&format!(
                "Can't list {}, assuming they're not enabled on this cluster.",
                R::plural(&())
            ));
            Ok(None)
        }
        Err(e) => {
            status_progress.failure(Some(&format!("failed to list {}", R::plural(&()))));
            Err(CliError::ListTargetsFailed(
                mirrord_kube::error::KubeApiError::KubeError(e),
            ))
        }
    }
}

/// Get a single `R` by name, or `Ok(None)` if it does not exist (or the kind is not served, which
/// also answers 404). Any other error gets converted to a `CliError`.
pub(crate) async fn get_resource_if_defined<R, P>(
    resource_api: &kube::Api<R>,
    name: &str,
    status_progress: &mut P,
) -> Result<Option<R>, CliError>
where
    R: Resource<DynamicType = ()> + Clone + Debug + DeserializeOwned,
    P: Progress,
{
    match resource_api.get_opt(name).await {
        Ok(found) => Ok(found),
        Err(e) => {
            status_progress.failure(Some(&format!("failed to get {}", R::plural(&()))));
            Err(CliError::ListTargetsFailed(
                mirrord_kube::error::KubeApiError::KubeError(e),
            ))
        }
    }
}

/// Kubernetes settings of the run's mirrord config, shown before the final CLI error by
/// [`crate::error::print_run_kube_environment`].
static RUN_KUBE_INPUTS: Mutex<Option<RunKubeInputs>> = Mutex::new(None);

/// Where the run's mirrord config came from.
#[derive(Debug, Default)]
pub(crate) enum ConfigSource {
    /// No config file was used.
    #[default]
    None,
    /// Config file at this path.
    File(String),
    /// Config resolved by the parent `mirrord up` process.
    MirrordUp,
}

/// Namespace the command works in.
#[derive(Debug, Default)]
pub(crate) enum RunNamespace {
    /// Every namespace (`-A`). Set from the command arguments when the environment is shown,
    /// because `-A` is not part of the mirrord config.
    All,
    /// `target.namespace` from the mirrord config, which `-n` sets.
    Named(String),
    /// Default namespace of the selected kube context.
    #[default]
    KubeDefault,
}

impl RunNamespace {
    fn of(config: &LayerConfig) -> Self {
        config
            .target
            .namespace
            .clone()
            .map(Self::Named)
            .unwrap_or(Self::KubeDefault)
    }
}

/// Kubernetes environment settings of the session.
#[derive(Debug, Default)]
pub(crate) struct RunKubeInputs {
    pub(crate) config_source: ConfigSource,
    /// `kubeconfig` from the mirrord config, or `KUBECONFIG` when it merges several files.
    pub(crate) kubeconfig: Option<String>,
    pub(crate) kube_context: Option<String>,
    pub(crate) namespace: RunNamespace,
}

/// The Kubernetes environment of a run, displayed by [`crate::error::print_run_kube_environment`].
#[derive(Debug)]
pub(crate) struct RunKubeEnvironment {
    /// Settings that selected the environment.
    pub(crate) inputs: RunKubeInputs,
    /// What the kubeconfig says about the selected context.
    pub(crate) resolved: Option<KubeEnvironment>,
}

/// Records the Kubernetes settings of the resolved `config`, so that a failing command can show
/// them. The kubeconfig is only read when the error is shown, see [`take_run_kube_environment`].
///
/// Only the first resolved config of the run is recorded, so a command that resolves its config
/// again does not replace it.
pub(crate) fn record_run_kube_environment(config_source: ConfigSource, config: &LayerConfig) {
    let mut recorded = lock_run_kube_inputs();
    if recorded.is_some() {
        return;
    }

    let kubeconfig = config.kubeconfig.clone().or_else(|| {
        std::env::var_os("KUBECONFIG")
            .filter(|paths| std::env::split_paths(paths).count() > 1)
            .map(|paths| paths.to_string_lossy().into_owned())
    });

    *recorded = Some(RunKubeInputs {
        config_source,
        kubeconfig,
        kube_context: config.kube_context.clone(),
        namespace: RunNamespace::of(config),
    });
}

/// Call after changing the target namespace of an already resolved `config`.
pub(crate) fn update_run_target_namespace(config: &LayerConfig) {
    if let Some(inputs) = lock_run_kube_inputs().as_mut() {
        inputs.namespace = RunNamespace::of(config);
    }
}

/// Takes the recorded settings and reads the kubeconfig for the context they select.
/// `all_namespaces` is whether the command was run with `-A`.
///
/// Reading the kubeconfig can be slow for large files, so it only happens here, on the error
/// path, and not every time a config is resolved.
pub(crate) fn take_run_kube_environment(all_namespaces: bool) -> Option<RunKubeEnvironment> {
    let mut inputs = lock_run_kube_inputs().take()?;
    if all_namespaces {
        inputs.namespace = RunNamespace::All;
    }
    let resolved = resolve_kube_environment(
        inputs.kubeconfig.as_deref(),
        inputs.kube_context.as_deref(),
    )
    .inspect_err(
        |error| tracing::debug!(%error, "failed to resolve kube environment for error reporting"),
    )
    .ok();

    Some(RunKubeEnvironment { inputs, resolved })
}

fn lock_run_kube_inputs() -> MutexGuard<'static, Option<RunKubeInputs>> {
    // This is best effort. Poisoned lock should not fail the command.
    RUN_KUBE_INPUTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

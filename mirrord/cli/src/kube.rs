use std::fmt::Debug;

use kube::{Config, Resource, api::ListParams, client::ClientBuilder};
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::kubernetes::create_kube_config, error::Result as KubeApiResult,
    retry::retry_policy_from_config,
};
use mirrord_operator::client::add_baggage_header;
use mirrord_progress::Progress;
use serde::de::DeserializeOwned;
use tower::{buffer::BufferLayer, retry::RetryLayer};

use crate::error::CliError;

/// Create a kube client according to the layer config, and with a request buffer of 1024 requests
/// and a retry policy according to the layer config. The configured baggage rides on every
/// request, since the commands using this client read operator resources that another operator
/// instance may be serving for that baggage, such as one run through mirrord on top of the
/// deployed one.
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

    build_client(config, layer_config)
        .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))
}

fn build_client(config: Config, layer_config: &LayerConfig) -> KubeApiResult<kube::Client> {
    Ok(ClientBuilder::try_from(config)?
        .with_layer(&BufferLayer::new(1024))
        .with_layer(&RetryLayer::new(retry_policy_from_config(
            &layer_config.startup_retry,
        )?))
        .build())
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

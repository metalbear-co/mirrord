use std::fmt::Debug;

use kube::{Resource, api::ListParams, client::ClientBuilder};
use mirrord_config::LayerConfig;
use mirrord_kube::{api::kubernetes::create_kube_config, retry::RetryKube};
use mirrord_progress::Progress;
use serde::de::DeserializeOwned;
use tower::{buffer::BufferLayer, retry::RetryLayer};

use crate::error::CliError;

/// Create a kube client according to the layer config, and with a request buffer of 1024 requests
/// and a retry policy according to the layer config.
pub(crate) async fn kube_client_from_layer_config(
    layer_config: &LayerConfig,
) -> Result<kube::Client, CliError> {
    create_kube_config(
        layer_config.accept_invalid_certificates,
        layer_config.kubeconfig.clone(),
        layer_config.kube_context.clone(),
    )
    .await
    .and_then(|config| {
        Ok(ClientBuilder::try_from(config.clone())?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(RetryKube::try_from(
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

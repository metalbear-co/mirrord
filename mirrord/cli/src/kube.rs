use kube::client::ClientBuilder;
use mirrord_config::LayerConfig;
use mirrord_kube::{api::kubernetes::create_kube_config, retry::RetryKube};
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

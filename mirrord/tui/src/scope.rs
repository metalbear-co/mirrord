use std::time::Duration;

use kube::Client;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_kube::api::kubernetes::create_kube_config;

/// The active context and namespace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Scope {
    /// Explicit context if set, current otherwise.
    pub context: Option<String>,
    /// Explicit namespace if set, default otherwise.
    pub namespace: Option<String>,
}

impl Scope {
    /// Builds a client for this scope.
    pub async fn build_client(&self) -> anyhow::Result<Client> {
        let mut config_context = ConfigContext::default();

        let layer_config = LayerConfig::resolve(&mut config_context)?;

        let mut kube_config = create_kube_config(
            layer_config.accept_invalid_certificates,
            layer_config.kubeconfig.clone(),
            self.context.clone(),
        )
        .await?;

        kube_config.connect_timeout = Some(Duration::from_secs(30));
        kube_config.read_timeout = Some(Duration::from_secs(30));
        kube_config.write_timeout = Some(Duration::from_secs(30));

        kube_config.default_retry = false;

        Ok(Client::try_from(kube_config)?)
    }
}

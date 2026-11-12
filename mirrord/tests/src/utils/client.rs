use std::sync::Once;

use kube::{Client, Config};
use rstest::fixture;

static CRYPTO_PROVIDER_INSTALLED: Once = Once::new();

/// A kube [`Client`] bundled with the [`Config`] it was created from.
///
/// Carrying the config lets cleanup code (running in a fresh runtime inside [`Drop`]) create a new
/// client pointed at the same cluster without calling [`Config::infer`] again — which would break
/// in multi-cluster setups where the active kubeconfig context may differ at deletion time.
///
/// Derefs to [`Client`] so it can be used where `&Client` is expected without an explicit field
/// access. For owned [`Client`] values (e.g. `Api::all(...)`) use `.get_client()`.
#[derive(Clone)]
pub struct KubeClient {
    client: Client,
    config: Config,
}

impl KubeClient {
    pub fn get_client(&self) -> Client {
        self.client.clone()
    }

    pub fn get_config(&self) -> Config {
        self.config.clone()
    }
}

impl TryFrom<Config> for KubeClient {
    type Error = <Client as TryFrom<Config>>::Error;

    fn try_from(config: Config) -> Result<Self, Self::Error> {
        Ok(Self {
            client: Client::try_from(config.clone())?,
            config,
        })
    }
}

impl std::ops::Deref for KubeClient {
    type Target = Client;

    fn deref(&self) -> &Client {
        &self.client
    }
}

/// Fixture that returns a [`KubeClient`] — a [`Client`] bundled with the [`Config`] used to
/// create it. Internal tests import this as `kube_client` via the module path:
///
/// ```rust,ignore
/// use crate::utils::kube_client::kube_client;
/// ```
///
/// External consumers that only need a plain [`Client`] can still import the top-level
/// `kube_client` from `crate::utils` (or `mirrord_tests::utils`), which wraps this fixture and
/// returns just the [`Client`].
#[fixture]
pub async fn kube_client() -> KubeClient {
    CRYPTO_PROVIDER_INSTALLED.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });

    // Bazel runs the tests with a `HOME` of their own, so they only find the cluster that
    // `--test_env=KUBECONFIG` names.
    let mut config = Config::infer().await.unwrap_or_else(|error| {
        panic!(
            "can't find the cluster to test against: {error}. Under Bazel, pass the kubeconfig that \
             `bazel run //mirrord/tests:e2e_setup` prints, with `--test_env=KUBECONFIG=<path>`."
        )
    });
    config.accept_invalid_certs = true;
    KubeClient::try_from(config).unwrap()
}

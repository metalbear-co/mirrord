use kube::{Client, Resource, api::GroupVersionKind, discovery};
use mirrord_kube::{RETRY_KUBE_OPERATIONS, RetryConfig};
use tokio_retry::{Action, RetryIf, strategy::jitter};
use tracing::Level;

use crate::crd::MirrordOperatorCrd;

#[tracing::instrument(level = Level::INFO, skip(action), err)]
async fn retry_discovery<T, A>(retry_config: Option<RetryConfig>, action: A) -> kube::Result<T>
where
    A: Action<Item = T, Error = kube::Error>,
{
    let RetryConfig {
        exponential_backoff,
        max_attempts,
    } = retry_config.unwrap_or_default();

    let retry_strategy = exponential_backoff.clone().map(jitter).take(max_attempts);

    let result = RetryIf::spawn(
        retry_strategy,
        action,
        (|fail| match fail {
            kube::Error::Api(response) if response.code == 404 => false,
            _ => {
                matches!(
                    fail,
                    kube::Error::HyperError(..)
                        | kube::Error::Service(..)
                        | kube::Error::HttpError(..)
                        | kube::Error::Auth(..)
                        | kube::Error::UpgradeConnection(..)
                        | kube::Error::Api(..)
                )
            }
        }) as fn(&A::Error) -> bool,
    )
    .await;

    Ok(result?)
}

#[tracing::instrument(level = Level::TRACE, skip_all, ret, err)]
pub async fn operator_installed(client: &Client) -> kube::Result<bool> {
    let gvk = GroupVersionKind {
        group: MirrordOperatorCrd::group(&()).into_owned(),
        version: MirrordOperatorCrd::version(&()).into_owned(),
        kind: MirrordOperatorCrd::kind(&()).into_owned(),
    };

    retry_discovery(RETRY_KUBE_OPERATIONS.get().cloned(), || async {
        match discovery::oneshot::pinned_kind(client, &gvk).await {
            Ok(..) => Ok(true),
            Err(kube::Error::Api(response)) if response.code == 404 => Ok(false),
            Err(error) => Err(error),
        }
    })
    .await
}

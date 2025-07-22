use kube::{api::GroupVersionKind, discovery, Client, Resource};

use crate::crd::MirrordOperatorCrd;

#[tracing::instrument(level = "trace", skip_all, ret, err)]
pub async fn operator_installed(client: &Client) -> kube::Result<bool> {
    let gvk = GroupVersionKind {
        group: MirrordOperatorCrd::group(&()).into_owned(),
        version: MirrordOperatorCrd::version(&()).into_owned(),
        kind: MirrordOperatorCrd::kind(&()).into_owned(),
    };

    match discovery::oneshot::pinned_kind(client, &gvk).await {
        Ok(..) => Ok(true),
        Err(kube::Error::Api(response)) if response.code == 404 => Ok(false),
        Err(error) => Err(error),
    }
}

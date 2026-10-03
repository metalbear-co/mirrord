use kube::{Client, Resource, api::GroupVersionKind, discovery};
use tracing::Level;

use crate::crd::MirrordOperatorCrd;

#[tracing::instrument(level = Level::TRACE, skip_all, ret, err)]
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

/// Operator resource that serves sessions-manager assignment streams.
const SESSION_ASSIGNMENTS_RESOURCE: &str = "sessionassignments";

/// Checks whether the operator serves its hosted sessions-manager routes. The operator lists
/// them in `v1alpha1` discovery only while the hosted sessions-manager is enabled.
#[tracing::instrument(level = Level::TRACE, skip_all, ret, err)]
pub async fn serverless_sessions_manager_served(client: &Client) -> kube::Result<bool> {
    let api_version = format!("{}/v1alpha1", MirrordOperatorCrd::group(&()));
    match client.list_api_group_resources(&api_version).await {
        Ok(list) => Ok(list
            .resources
            .iter()
            .any(|resource| resource.name == SESSION_ASSIGNMENTS_RESOURCE)),
        // An operator that predates `v1alpha1` serves no sessions-manager routes either.
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(false),
        Err(error) => Err(error),
    }
}

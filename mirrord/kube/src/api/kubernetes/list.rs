//! Helper functions for listing k8s [`Resource`]s.

use std::fmt;

use futures::Stream;
use k8s_openapi::{ClusterResourceScope, NamespaceResourceScope};
use kube::{api::ListParams, Api, Client, Resource};
use serde::de::DeserializeOwned;

/// Prepares [`ListParams`] that:
/// 1. Excludes our own resources
/// 2. Adds a limit for item count in a response
fn make_list_params(field_selector: Option<&str>) -> ListParams {
    ListParams {
        label_selector: Some("app!=mirrord,!operator.metalbear.co/owner".to_string()),
        field_selector: field_selector.map(ToString::to_string),
        limit: Some(500),
        ..Default::default()
    }
}

/// Returns a [`Stream`] of all objects in the given namespace.
///
/// 1. `field_selector` can be used for filtering.
/// 2. Our own resources are excluded.
pub fn list_all_namespaced<R>(
    client: Client,
    namespace: &str,
    field_selector: Option<&str>,
) -> impl 'static + Stream<Item = kube::Result<R>> + Send
where
    R: 'static
        + Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + fmt::Debug
        + Clone
        + DeserializeOwned
        + Send,
{
    let api = Api::namespaced(client, namespace);
    let mut params = make_list_params(field_selector);

    async_stream::stream! {
        loop {
            let response = api.list(&params).await?;

            for resource in response.items {
                yield Ok(resource);
            }

            let continue_token = response.metadata.continue_.unwrap_or_default();
            if continue_token.is_empty() {
                break;
            }
            params.continue_token.replace(continue_token);
        }
    }
}

/// Returns a [`Stream`] of all objects in the cluster.
///
/// 1. `field_selector` can be used for filtering.
/// 2. Our own resources are excluded.
pub fn list_all_clusterwide<R>(
    client: Client,
    field_selector: Option<&str>,
) -> impl 'static + Stream<Item = kube::Result<R>> + Send
where
    R: 'static
        + Resource<DynamicType = (), Scope = ClusterResourceScope>
        + fmt::Debug
        + Clone
        + DeserializeOwned
        + Send,
{
    let api = Api::all(client);
    let mut params = make_list_params(field_selector);

    async_stream::stream! {
        loop {
            let response = api.list(&params).await?;

            for resource in response.items {
                yield Ok(resource);
            }

            let continue_token = response.metadata.continue_.unwrap_or_default();
            if continue_token.is_empty() {
                break;
            }
            params.continue_token.replace(continue_token);
        }
    }
}

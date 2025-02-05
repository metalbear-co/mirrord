use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::core::v1::Pod;
use kube::{api::ListParams, Api, Client, Resource};

use super::ResolvedResource;
use crate::{
    api::{kubernetes::rollout::Rollout, runtime::RuntimeDataFromLabels},
    error::{KubeApiError, Result},
    resolved::get_k8s_resource_api,
};

impl RuntimeDataFromLabels for ResolvedResource<Rollout> {
    type Resource = Rollout;

    fn name(&self) -> Cow<str> {
        self.resource
            .metadata
            .name
            .as_ref()
            .map(Cow::from)
            .unwrap_or_default()
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    fn get_selector_match_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.as_ref())
            .and_then(|selector| selector.match_labels.clone())
            .ok_or_else(|| {
                KubeApiError::missing_field(
                    resource,
                    ".spec.selector or .spec.selector.match_labels",
                )
            })
    }

    async fn get_pods(resource: &Self::Resource, client: &Client) -> Result<Vec<Pod>> {
        let formatted_labels = resource
            .get_match_labels(client)
            .await?
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<String>>()
            .join(",");

        let list_params = ListParams {
            label_selector: Some(formatted_labels),
            ..Default::default()
        };

        let pod_api: Api<Pod> = get_k8s_resource_api(client, resource.meta().namespace.as_deref());
        let pods = pod_api.list(&list_params).await?;

        Ok(pods.items)
    }
}

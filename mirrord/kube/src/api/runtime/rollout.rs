use std::borrow::Cow;

use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client, Resource, api::ListParams, core::Selector};
use mirrord_config::target::rollout::RolloutTarget;

use super::RuntimeDataFromLabels;
use crate::{
    api::{kubernetes::rollout::Rollout, runtime::get_k8s_resource_api},
    error::{KubeApiError, Result},
};

impl RuntimeDataFromLabels for RolloutTarget {
    type Resource = Rollout;

    fn name(&self) -> Cow<'_, str> {
        Cow::from(&self.rollout)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    /// Uses the inline selector; workload references require the async pod-resolution path.
    fn get_selector(resource: &Self::Resource) -> Result<Selector> {
        let selector = resource
            .spec
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec"))?
            .selector
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector"))?;
        if selector.match_labels.is_none()
            && selector
                .match_expressions
                .as_ref()
                .is_none_or(Vec::is_empty)
        {
            return Err(KubeApiError::missing_field(
                resource,
                ".spec.selector.matchLabels",
            ));
        }

        Ok(Selector::try_from(selector.clone())?)
    }

    // Workload references require an async fetch to resolve the complete selector.
    async fn get_pods(resource: &Self::Resource, client: &Client) -> Result<Vec<Pod>> {
        let selector = resource.get_match_labels(client).await?;
        if selector.match_labels.is_none()
            && selector
                .match_expressions
                .as_ref()
                .is_none_or(Vec::is_empty)
        {
            return Err(KubeApiError::missing_field(
                resource,
                ".selector or .selector.match_labels",
            ));
        }
        let selector = Selector::try_from(selector.into_owned())?;

        let list_params = ListParams::default().labels_from(&selector);

        let pod_api: Api<Pod> = get_k8s_resource_api(client, resource.meta().namespace.as_deref());
        let pods = pod_api.list(&list_params).await?;

        Ok(pods.items)
    }
}

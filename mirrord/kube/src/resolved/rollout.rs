use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use mirrord_config::target::rollout::RolloutTarget;

use super::ResolvedResource;
use crate::{
    api::{kubernetes::rollout::Rollout, runtime::RuntimeDataFromLabels},
    error::Result,
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
        RolloutTarget::get_selector_match_labels(resource)
    }

    // Override auto implementaion because `LabelSelector` needs to be async fetched for Rollout
    async fn get_pods(resource: &Self::Resource, client: &Client) -> Result<Vec<Pod>> {
        RolloutTarget::get_pods(resource, client).await
    }
}

use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::apps::v1::Deployment;

use super::ResolvedResource;
use crate::{api::runtime::RuntimeDataFromLabels, error::KubeApiError};

impl RuntimeDataFromLabels for ResolvedResource<Deployment> {
    type Resource = Deployment;

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

    async fn get_labels(
        resource: &Self::Resource,
    ) -> Result<BTreeMap<String, String>, KubeApiError> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.match_labels.clone())
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

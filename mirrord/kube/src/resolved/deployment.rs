use std::borrow::Cow;

use k8s_openapi::api::apps::v1::Deployment;
use kube::core::Selector;

use super::ResolvedResource;
use crate::{
    api::runtime::{RuntimeDataFromLabels, has_match_labels_or_expressions},
    error::KubeApiError,
};

impl RuntimeDataFromLabels for ResolvedResource<Deployment> {
    type Resource = Deployment;

    fn name(&self) -> Cow<'_, str> {
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

    fn get_selector(resource: &Self::Resource) -> Result<Selector, KubeApiError> {
        let selector = resource
            .spec
            .as_ref()
            .map(|spec| &spec.selector)
            .filter(|selector| has_match_labels_or_expressions(selector))
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))?;

        Ok(Selector::try_from(selector.clone())?)
    }
}

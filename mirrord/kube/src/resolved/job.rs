use std::borrow::Cow;

use k8s_openapi::api::batch::v1::Job;
use kube::core::Selector;

use super::ResolvedResource;
use crate::{
    api::runtime::{RuntimeDataFromLabels, has_label_selector},
    error::{KubeApiError, Result},
};

impl RuntimeDataFromLabels for ResolvedResource<Job> {
    type Resource = Job;

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

    fn get_selector(resource: &Self::Resource) -> Result<Selector> {
        let selector = resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.as_ref())
            .filter(|selector| has_label_selector(selector))
            .ok_or_else(|| {
                KubeApiError::missing_field(
                    resource,
                    ".spec.selector or .spec.selector.match_labels",
                )
            })?;

        Ok(Selector::try_from(selector.clone())?)
    }
}

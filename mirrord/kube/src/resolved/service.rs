use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::core::v1::Service;

use super::{ResolvedResource, RuntimeDataFromLabels};
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for ResolvedResource<Service> {
    type Resource = Service;

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
            .and_then(|spec| spec.selector.clone())
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector"))
    }
}

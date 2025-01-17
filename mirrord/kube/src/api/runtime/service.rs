use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::core::v1::Service;
use mirrord_config::target::service::ServiceTarget;

use super::RuntimeDataFromLabels;
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for ServiceTarget {
    type Resource = Service;

    fn name(&self) -> Cow<str> {
        Cow::from(&self.service)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    async fn get_selector_match_labels(
        resource: &Self::Resource,
    ) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.clone())
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector"))
    }
}

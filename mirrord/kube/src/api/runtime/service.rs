use std::borrow::Cow;

use k8s_openapi::api::core::v1::Service;
use kube::core::Selector;
use mirrord_config::target::service::ServiceTarget;

use super::RuntimeDataFromLabels;
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for ServiceTarget {
    type Resource = Service;

    fn name(&self) -> Cow<'_, str> {
        Cow::from(&self.service)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    fn get_selector(resource: &Self::Resource) -> Result<Selector> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.clone())
            .map(|selector| selector.into_iter().collect())
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector"))
    }
}

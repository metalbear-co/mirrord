use std::{borrow::Cow, ops::Not};

use k8s_openapi::api::apps::v1::Deployment;
use kube::core::Selector;
use mirrord_config::target::deployment::DeploymentTarget;

use super::RuntimeDataFromLabels;
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for DeploymentTarget {
    type Resource = Deployment;

    fn name(&self) -> Cow<'_, str> {
        Cow::from(&self.deployment)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    fn get_selector(resource: &Self::Resource) -> Result<Selector> {
        let selector = resource
            .spec
            .as_ref()
            .map(|spec| &spec.selector)
            .filter(|selector| {
                selector.match_labels.is_some()
                    || selector
                        .match_expressions
                        .as_ref()
                        .is_some_and(|expressions| expressions.is_empty().not())
            })
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))?;

        Ok(Selector::try_from(selector.clone())?)
    }
}

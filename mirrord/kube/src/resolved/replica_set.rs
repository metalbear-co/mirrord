use std::{borrow::Cow, ops::Not};

use k8s_openapi::api::apps::v1::ReplicaSet;
use kube::core::Selector;

use super::{ResolvedResource, RuntimeDataFromLabels};
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for ResolvedResource<ReplicaSet> {
    type Resource = ReplicaSet;

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

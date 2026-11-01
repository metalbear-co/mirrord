use std::borrow::Cow;

use k8s_openapi::api::apps::v1::StatefulSet;
use kube::core::Selector;
use mirrord_config::target::stateful_set::StatefulSetTarget;

use super::{RuntimeDataFromLabels, has_match_labels_or_expressions};
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for StatefulSetTarget {
    type Resource = StatefulSet;

    fn name(&self) -> Cow<'_, str> {
        Cow::from(&self.stateful_set)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    fn get_selector(resource: &Self::Resource) -> Result<Selector> {
        let selector = resource
            .spec
            .as_ref()
            .map(|spec| &spec.selector)
            .filter(|selector| has_match_labels_or_expressions(selector))
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))?;

        Ok(Selector::try_from(selector.clone())?)
    }
}

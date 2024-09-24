use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::apps::v1::StatefulSet;
use mirrord_config::target::stateful_set::StatefulSetTarget;

use super::RuntimeDataFromLabels;
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for StatefulSetTarget {
    type Resource = StatefulSet;

    fn name(&self) -> Cow<str> {
        Cow::from(&self.stateful_set)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    async fn get_selector_match_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.match_labels.clone())
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

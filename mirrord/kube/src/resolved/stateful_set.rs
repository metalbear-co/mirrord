use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::StatefulSet;

use super::{ResolvedResource, RuntimeDataFromLabels};
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for ResolvedResource<StatefulSet> {
    type Resource = StatefulSet;

    fn name(&self) -> &str {
        self.resource.metadata.name.as_ref().unwrap().as_str()
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    async fn get_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.match_labels.clone())
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

use std::collections::BTreeMap;

use crate::{
    api::{kubernetes::rollout::Rollout, runtime::RuntimeDataFromLabels},
    error::{KubeApiError, Result},
};

#[derive(Debug, Clone)]
pub struct ResolvedRollout {
    pub resource: Rollout,
    pub container: Option<String>,
}

impl RuntimeDataFromLabels for ResolvedRollout {
    type Resource = Rollout;

    fn name(&self) -> &str {
        self.resource.metadata.name.as_ref().unwrap().as_str()
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    /// Digs into `resource` to return its `.spec.selector.matchLabels`.
    async fn get_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .clone()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec"))?
            .selector
            .match_labels
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

use std::collections::BTreeMap;

use super::ResolvedResource;
use crate::{
    api::{kubernetes::rollout::Rollout, runtime::RuntimeDataFromLabels},
    error::{KubeApiError, Result},
};

impl RuntimeDataFromLabels for ResolvedResource<Rollout> {
    type Resource = Rollout;

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
            .ok_or_else(|| {
                KubeApiError::missing_field(
                    resource,
                    ".spec.selector or .spec.selector.match_labels",
                )
            })
    }
}
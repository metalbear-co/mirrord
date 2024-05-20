use std::collections::BTreeMap;

use mirrord_config::target::rollout::RolloutTarget;

use super::RuntimeDataFromLabels;
use crate::{
    api::kubernetes::rollout::Rollout,
    error::{KubeApiError, Result},
};

impl RuntimeDataFromLabels for RolloutTarget {
    type Resource = Rollout;

    fn name(&self) -> &str {
        &self.rollout
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    async fn get_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .match_labels()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

use std::{borrow::Cow, collections::BTreeMap};

use mirrord_config::target::rollout::RolloutTarget;

use super::RuntimeDataFromLabels;
use crate::{
    api::kubernetes::rollout::Rollout,
    error::{KubeApiError, Result},
};

impl RuntimeDataFromLabels for RolloutTarget {
    type Resource = Rollout;

    fn name(&self) -> Cow<str> {
        Cow::from(&self.rollout)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    /// Digs into `resource` to return its `.spec.selector.matchLabels`.
    async fn get_selector_match_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .clone()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec"))?
            .selector
            .match_labels
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

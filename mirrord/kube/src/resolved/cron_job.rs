use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::batch::v1::CronJob;

use super::ResolvedResource;
use crate::{
    api::runtime::RuntimeDataFromLabels,
    error::{KubeApiError, Result},
};

impl RuntimeDataFromLabels for ResolvedResource<CronJob> {
    type Resource = CronJob;

    fn name(&self) -> Cow<str> {
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

    fn get_selector_match_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| {
                spec.job_template
                    .spec
                    .as_ref()?
                    .selector
                    .as_ref()?
                    .match_labels
                    .clone()
            })
            .ok_or_else(|| {
                KubeApiError::missing_field(
                    resource,
                    ".spec.selector or .spec.selector.matchLabels",
                )
            })
    }
}

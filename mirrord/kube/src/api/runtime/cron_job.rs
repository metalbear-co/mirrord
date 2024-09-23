use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::batch::v1::CronJob;
use mirrord_config::target::cron_job::CronJobTarget;

use super::RuntimeDataFromLabels;
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for CronJobTarget {
    type Resource = CronJob;

    fn name(&self) -> Cow<str> {
        Cow::from(&self.cron_job)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    async fn get_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
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

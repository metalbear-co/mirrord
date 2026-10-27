use std::borrow::Cow;

use k8s_openapi::api::batch::v1::CronJob;
use kube::core::Selector;
use mirrord_config::target::cron_job::CronJobTarget;

use super::{RuntimeDataFromLabels, has_match_labels_or_expressions};
use crate::error::{KubeApiError, Result};

impl RuntimeDataFromLabels for CronJobTarget {
    type Resource = CronJob;

    fn name(&self) -> Cow<'_, str> {
        Cow::from(&self.cron_job)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    fn get_selector(resource: &Self::Resource) -> Result<Selector> {
        let selector = resource
            .spec
            .as_ref()
            .and_then(|spec| spec.job_template.spec.as_ref()?.selector.as_ref())
            .filter(|selector| has_match_labels_or_expressions(selector))
            .ok_or_else(|| {
                KubeApiError::missing_field(
                    resource,
                    ".spec.selector or .spec.selector.matchLabels",
                )
            })?;

        Ok(Selector::try_from(selector.clone())?)
    }
}

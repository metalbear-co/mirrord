use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::Job;
use kube::{Api, Client};
use mirrord_config::target::job::JobTarget;

use super::{RuntimeDataFromLabels, RuntimeTarget};
use crate::{
    api::kubernetes::get_k8s_resource_api,
    error::{KubeApiError, Result},
};

impl RuntimeTarget for JobTarget {
    fn target(&self) -> &str {
        &self.job
    }

    fn container(&self) -> &Option<String> {
        &self.container
    }
}

impl RuntimeDataFromLabels for JobTarget {
    async fn get_labels(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> Result<BTreeMap<String, String>> {
        let job_api: Api<Job> = get_k8s_resource_api(client, namespace);
        let job = job_api
            .get(&self.job)
            .await
            .map_err(KubeApiError::KubeError)?;

        job.spec
            .and_then(|spec| spec.selector?.match_labels)
            .ok_or_else(|| {
                KubeApiError::JobNotFound(format!(
                    "Label for job: {}, not found!",
                    self.job.clone()
                ))
            })
    }
}

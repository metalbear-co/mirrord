use std::collections::BTreeMap;

use kube::{Api, Client};
use mirrord_config::target::rollout::RolloutTarget;

use super::{RuntimeDataFromLabels, RuntimeTarget};
use crate::{
    api::kubernetes::{get_k8s_resource_api, rollout::Rollout},
    error::{KubeApiError, Result},
};

impl RuntimeTarget for RolloutTarget {
    fn target(&self) -> &str {
        &self.rollout
    }

    fn container(&self) -> &Option<String> {
        &self.container
    }
}

impl RuntimeDataFromLabels for RolloutTarget {
    async fn get_labels(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> Result<BTreeMap<String, String>> {
        let rollout_api: Api<Rollout> = get_k8s_resource_api(client, namespace);
        let rollout = rollout_api
            .get(&self.rollout)
            .await
            .map_err(KubeApiError::KubeError)?;

        rollout.match_labels().ok_or_else(|| {
            KubeApiError::DeploymentNotFound(format!(
                "Label for rollout: {}, not found!",
                self.rollout.clone()
            ))
        })
    }
}

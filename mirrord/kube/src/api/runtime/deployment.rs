use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use kube::{Api, Client};
use mirrord_config::target::deployment::DeploymentTarget;

use super::{RuntimeDataFromLabels, RuntimeTarget};
use crate::{
    api::kubernetes::get_k8s_resource_api,
    error::{KubeApiError, Result},
};

impl RuntimeTarget for DeploymentTarget {
    fn target(&self) -> &str {
        &self.deployment
    }

    fn container(&self) -> &Option<String> {
        &self.container
    }
}

impl RuntimeDataFromLabels for DeploymentTarget {
    async fn get_labels(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> Result<BTreeMap<String, String>> {
        let deployment_api: Api<Deployment> = get_k8s_resource_api(client, namespace);
        let deployment = deployment_api
            .get(&self.deployment)
            .await
            .map_err(KubeApiError::KubeError)?;

        deployment
            .spec
            .and_then(|spec| spec.selector.match_labels)
            .ok_or_else(|| {
                KubeApiError::DeploymentNotFound(format!(
                    "Label for deployment: {}, not found!",
                    self.deployment.clone()
                ))
            })
    }
}

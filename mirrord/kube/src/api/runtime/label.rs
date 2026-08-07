use k8s_openapi::api::core::v1::Pod;
use kube::{Client, api::ListParams};
use mirrord_config::target::label::LabelTarget;

use super::{RuntimeData, RuntimeDataProvider};
use crate::{
    api::kubernetes::get_k8s_resource_api,
    error::{KubeApiError, Result},
};

impl RuntimeDataProvider for LabelTarget {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        get_k8s_resource_api::<Pod>(client, namespace)
            .list(&ListParams::default().labels(&self.selector()))
            .await?
            .items
            .into_iter()
            .find_map(|pod| RuntimeData::from_pod(&pod, self.container.as_deref()).ok())
            .ok_or_else(|| {
                KubeApiError::InvalidTargetState(format!(
                    "no target-ready pod matching label selector `{}` was found",
                    self.selector()
                ))
            })
    }
}

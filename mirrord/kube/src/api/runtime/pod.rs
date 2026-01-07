use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client};
use mirrord_config::target::pod::PodTarget;

use super::{RuntimeData, RuntimeDataProvider};
use crate::{
    api::{ext::ApiExt, kubernetes::get_k8s_resource_api},
    error::Result,
};

impl RuntimeDataProvider for PodTarget {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let pod_api: Api<Pod> = get_k8s_resource_api(client, namespace);
        let pod = match pod_api.get_opt(&self.pod).await? {
            Some(pod) => pod,
            None => pod_api.search_one(&self.pod).await?,
        };

        RuntimeData::from_pod(&pod, self.container.as_deref())
    }
}

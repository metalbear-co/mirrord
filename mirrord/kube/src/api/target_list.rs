use k8s_openapi::api::core::v1::Pod;
use kube::{api::ListParams, Api};

use super::{get_k8s_api, kubernetes::create_kube_api};
use crate::error::Result;

pub async fn get_kube_pods(namespace: Option<&str>) -> Result<Vec<(String, Vec<String>)>> {
    let client = create_kube_api(&None).await?;
    let api: Api<Pod> = get_k8s_api(&client, namespace);
    let pods = api.list(&ListParams::default()).await?;

    // convert pods to (name, container names) pairs

    let pod_containers: Vec<(String, Vec<String>)> = pods
        .items
        .iter()
        .filter_map(|pod| {
            let name = pod.metadata.name.clone()?;
            let containers = pod
                .spec
                .as_ref()?
                .containers
                .iter()
                .map(|container| container.name.clone())
                .collect();
            Some((name, containers))
        })
        .collect();

    Ok(pod_containers)
}

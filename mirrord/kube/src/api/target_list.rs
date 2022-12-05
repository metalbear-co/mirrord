use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::{api::ListParams, Api, Client};
use serde_json::json;

use crate::error::Result;

async fn get_kube_namespaced_pods(namespace: &Option<&str>) -> Result<Vec<(String, Vec<String>)>> {
    let client = Client::try_default().await?;
    let namespace = namespace.unwrap_or("default");
    let pods: Api<Pod> = Api::namespaced(client, namespace);
    let pods = pods.list(&ListParams::default()).await?;

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

async fn get_kube_namespaces() -> Result<Vec<String>> {
    let client = Client::try_default().await?;
    let namespaces: Api<Namespace> = Api::all(client);
    let namespaces = namespaces.list(&ListParams::default()).await?;

    // convert namespaces to a list of names

    let namespace_names: Vec<String> = namespaces
        .items
        .iter()
        .filter_map(|namespace| namespace.metadata.name.clone())
        .collect();

    Ok(namespace_names)
}

async fn create_json(kube_objectlist: Vec<(String, Vec<String>)>) -> Result<String> {
    let json_obj = json!(kube_objectlist);
    Ok(json_obj.to_string())
}

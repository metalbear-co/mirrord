use anyhow::{anyhow, Result};
use async_trait::async_trait;
use containerd_client::{
    connect,
    services::v1::{containers_client::ContainersClient, GetContainerRequest},
    tonic::Request,
    with_namespace,
};

use crate::namespace::{Namespace, Spec};

pub struct ContainerdRuntime {
    container_id: String,
    containerd_socket_path: String,
    default_containerd_namespace: String,
}

impl ContainerdRuntime {
    pub fn new(container_id: String) -> Self {
        ContainerdRuntime {
            container_id,
            containerd_socket_path: "/run/containerd/containerd.sock".to_string(),
            default_containerd_namespace: "k8s.io".to_string(),
        }
    }
}

#[async_trait]
impl Namespace for ContainerdRuntime {
    async fn get_namespace(&self) -> Result<String> {
        let channel = connect(&self.containerd_socket_path).await?;
        let mut client = ContainersClient::new(channel);
        let request = GetContainerRequest {
            id: self.container_id.clone(),
        };
        let request = with_namespace!(request, self.default_containerd_namespace);
        let resp = client.get(request).await?;
        let resp = resp.into_inner();
        let container = resp
            .container
            .ok_or_else(|| anyhow!("container not found"))?;
        let spec: Spec = serde_json::from_slice(
            &container
                .spec
                .ok_or_else(|| anyhow!("invalid data from containerd"))?
                .value,
        )?;
        let ns_path = spec
            .linux
            .namespaces
            .iter()
            .find(|ns| ns.ns_type == "network")
            .ok_or_else(|| anyhow!("network namespace not found"))?
            .path
            .as_ref()
            .ok_or_else(|| anyhow!("no network namespace path"))?;
        Ok(ns_path.to_owned())
    }
}

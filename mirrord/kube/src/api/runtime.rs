use std::fmt::{Display, Formatter};

use k8s_openapi::api::core::v1::Pod;

use crate::{
    api::container::choose_container,
    error::{KubeApiError, Result},
};

#[derive(Debug)]
pub enum ContainerRuntime {
    Docker,
    Containerd,
}

impl ContainerRuntime {
    pub fn mount_path(&self) -> &str {
        match self {
            ContainerRuntime::Docker => "/var/run/docker.sock",
            ContainerRuntime::Containerd => "/run/",
        }
    }
}

impl Display for ContainerRuntime {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerRuntime::Docker => write!(f, "docker"),
            ContainerRuntime::Containerd => write!(f, "containerd"),
        }
    }
}

#[derive(Debug)]
pub struct RuntimeData {
    pub pod_name: String,
    pub node_name: String,
    pub container_id: String,
    pub container_runtime: ContainerRuntime,
    pub container_name: String,
}

impl RuntimeData {
    fn from_pod(pod: &Pod, container_name: &Option<String>) -> Result<Self> {
        let pod_name = pod
            .metadata
            .name
            .as_ref()
            .ok_or(KubeApiError::PodNameNotFound)?
            .to_owned();
        let node_name = pod
            .spec
            .as_ref()
            .ok_or(KubeApiError::PodSpecNotFound)?
            .node_name
            .as_ref()
            .ok_or(KubeApiError::NodeNotFound)?
            .to_owned();
        let container_statuses = pod
            .status
            .as_ref()
            .ok_or(KubeApiError::PodStatusNotFound)?
            .container_statuses
            .as_ref()
            .ok_or(KubeApiError::ContainerStatusNotFound)?;
        let chosen_status =
            choose_container(container_name, container_statuses).ok_or_else(|| {
                KubeApiError::ContainerNotFound(
                    container_name.clone().unwrap_or_else(|| "None".to_string()),
                )
            })?;

        let container_name = chosen_status.name.clone();
        let container_id_full = chosen_status
            .container_id
            .as_ref()
            .ok_or(KubeApiError::ContainerIdNotFound)?
            .to_owned();

        let mut split = container_id_full.split("://");

        let container_runtime = match split.next() {
            Some("docker") => ContainerRuntime::Docker,
            Some("containerd") => ContainerRuntime::Containerd,
            _ => {
                return Err(KubeApiError::ContainerRuntimeParseError(
                    container_id_full.to_string(),
                ))
            }
        };

        let container_id = split
            .next()
            .ok_or_else(|| KubeApiError::ContainerRuntimeParseError(container_id_full.to_string()))?
            .to_owned();

        Ok(RuntimeData {
            pod_name,
            node_name,
            container_id,
            container_runtime,
            container_name,
        })
    }
}

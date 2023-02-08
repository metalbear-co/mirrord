use std::fmt::{Display, Formatter};

use async_trait::async_trait;
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Pod};
use kube::{api::ListParams, Api, Client};
use mirrord_config::target::{DeploymentTarget, PodTarget, Target};

use crate::{
    api::{container::choose_container, get_k8s_resource_api},
    error::{KubeApiError, Result},
};

#[derive(Debug)]
pub enum ContainerRuntime {
    Docker,
    Containerd,
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

#[async_trait]
pub trait RuntimeDataProvider {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData>;
}

#[async_trait]
impl RuntimeDataProvider for Target {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        match self {
            Target::Deployment(deployment) => deployment.runtime_data(client, namespace).await,
            Target::Pod(pod) => pod.runtime_data(client, namespace).await,
        }
    }
}

#[async_trait]
impl RuntimeDataProvider for DeploymentTarget {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let deployment_api: Api<Deployment> = get_k8s_resource_api(client, namespace);
        let deployment = deployment_api
            .get(&self.deployment)
            .await
            .map_err(KubeApiError::KubeError)?;

        let deployment_labels = deployment
            .spec
            .and_then(|spec| spec.selector.match_labels)
            .ok_or_else(|| {
                KubeApiError::DeploymentNotFound(format!(
                    "Label for deployment: {}, not found!",
                    self.deployment.clone()
                ))
            })?;

        // convert to key value pair
        let formatted_deployments_labels = deployment_labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<String>>()
            .join(",");

        let pod_api: Api<Pod> = get_k8s_resource_api(client, namespace);
        let deployment_pods = pod_api
            .list(&ListParams::default().labels(&formatted_deployments_labels))
            .await
            .map_err(KubeApiError::KubeError)?;

        let first_pod = deployment_pods.items.first().ok_or_else(|| {
            KubeApiError::DeploymentNotFound(format!(
                "Failed to fetch the default(first pod) from ObjectList<Pod> for {}",
                self.deployment.clone()
            ))
        })?;

        RuntimeData::from_pod(first_pod, &self.container)
    }
}

#[async_trait]
impl RuntimeDataProvider for PodTarget {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let pod_api: Api<Pod> = get_k8s_resource_api(client, namespace);
        let pod = pod_api.get(&self.pod).await?;

        RuntimeData::from_pod(&pod, &self.container)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("pod/foobaz", Target::Pod(PodTarget {pod: "foobaz".to_string(), container: None}))]
    #[case("deployment/foobaz", Target::Deployment(DeploymentTarget {deployment: "foobaz".to_string(), container: None}))]
    #[case("deployment/nginx-deployment", Target::Deployment(DeploymentTarget {deployment: "nginx-deployment".to_string(), container: None}))]
    #[case("pod/foo/container/baz", Target::Pod(PodTarget { pod: "foo".to_string(), container: Some("baz".to_string()) }))]
    #[case("deployment/nginx-deployment/container/container-name", Target::Deployment(DeploymentTarget {deployment: "nginx-deployment".to_string(), container: Some("container-name".to_string())}))]
    fn test_target_parses(#[case] target: &str, #[case] expected: Target) {
        let target = target.parse::<Target>().unwrap();
        assert_eq!(target, expected)
    }

    #[rstest]
    #[should_panic(expected = "InvalidTarget")]
    #[case::panic("deployment/foobaz/blah")]
    #[should_panic(expected = "InvalidTarget")]
    #[case::panic("pod/foo/baz")]
    fn test_target_parse_fails(#[case] target: &str) {
        let target = target.parse::<Target>().unwrap();
        assert_eq!(
            target,
            Target::Deployment(DeploymentTarget {
                deployment: "foobaz".to_string(),
                container: None
            })
        )
    }
}

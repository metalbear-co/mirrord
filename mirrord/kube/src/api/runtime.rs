use std::{
    collections::BTreeMap,
    convert::Infallible,
    fmt::{Display, Formatter},
    ops::FromResidual,
};

use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Node, Pod},
    },
    apimachinery::pkg::api::resource::Quantity,
};
use kube::{api::ListParams, Api, Client};
use mirrord_config::target::{DeploymentTarget, PodTarget, RolloutTarget, Target};
use mirrord_protocol::MeshVendor;

use crate::{
    api::{
        container::choose_container,
        kubernetes::{get_k8s_resource_api, rollout::Rollout},
    },
    error::{KubeApiError, Result},
};

#[derive(Debug)]
pub enum ContainerRuntime {
    Docker,
    Containerd,
    CriO,
}

impl Display for ContainerRuntime {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerRuntime::Docker => write!(f, "docker"),
            ContainerRuntime::Containerd => write!(f, "containerd"),
            ContainerRuntime::CriO => write!(f, "cri-o"),
        }
    }
}

#[derive(Debug)]
pub struct RuntimeData {
    pub pod_name: String,
    pub pod_namespace: Option<String>,
    pub node_name: String,
    pub container_id: String,
    pub container_runtime: ContainerRuntime,
    pub container_name: String,

    /// Used to check if we're running with a mesh/sidecar in `detect_mesh_mirror_mode`.
    pub mesh: Option<MeshVendor>,
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
            .clone()
            .ok_or(KubeApiError::ContainerStatusNotFound)?;
        let (chosen_container, mesh) =
            choose_container(container_name, container_statuses.as_ref());

        let chosen_status = chosen_container.ok_or_else(|| {
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
            Some("cri-o") => ContainerRuntime::CriO,
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
            pod_namespace: pod.metadata.namespace.clone(),
            node_name,
            container_id,
            container_runtime,
            container_name,
            mesh,
        })
    }

    #[tracing::instrument(level = "trace", skip(client), ret)]
    pub async fn check_node(&self, client: &kube::Client) -> NodeCheck {
        let node_api: Api<Node> = Api::all(client.clone());
        let pod_api: Api<Pod> = Api::all(client.clone());

        let node = node_api.get(&self.node_name).await?;

        let allocatable = node
            .status
            .and_then(|status| status.allocatable)
            .ok_or_else(|| KubeApiError::NodeBadAllocatable(self.node_name.clone()))?;

        let allowed: usize = allocatable
            .get("pods")
            .and_then(|Quantity(quantity)| quantity.parse().ok())
            .ok_or_else(|| KubeApiError::NodeBadAllocatable(self.node_name.clone()))?;

        let mut pod_count = 0;
        let mut list_params = ListParams {
            field_selector: Some(format!("spec.nodeName={}", self.node_name)),
            ..Default::default()
        };

        loop {
            let pods_on_node = pod_api.list(&list_params).await?;

            pod_count += pods_on_node.items.len();

            match pods_on_node.metadata.continue_ {
                Some(next) => {
                    list_params = list_params.continue_token(&next);
                }
                None => break,
            }
        }

        if allowed <= pod_count {
            NodeCheck::Failed(self.node_name.clone(), pod_count)
        } else {
            NodeCheck::Success
        }
    }
}

#[derive(Debug)]
pub enum NodeCheck {
    Success,
    Failed(String, usize),
    Error(KubeApiError),
}

impl<E> FromResidual<Result<Infallible, E>> for NodeCheck
where
    E: Into<KubeApiError>,
{
    fn from_residual(error: Result<Infallible, E>) -> Self {
        match error {
            Ok(_) => unreachable!(),
            Err(err) => match err.into() {
                KubeApiError::NodePodLimitExceeded(node_name, pods) => {
                    NodeCheck::Failed(node_name, pods)
                }
                err => NodeCheck::Error(err),
            },
        }
    }
}

pub trait RuntimeDataProvider {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData>;
}

pub trait RuntimeTarget {
    fn target(&self) -> &str;

    fn container(&self) -> &Option<String>;
}

pub trait RuntimeDataFromLabels {
    async fn get_labels(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> Result<BTreeMap<String, String>>;
}

impl<T> RuntimeDataProvider for T
where
    T: RuntimeTarget + RuntimeDataFromLabels,
{
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let labels = self.get_labels(client, namespace).await?;

        // convert to key value pair
        let formatted_deployments_labels = labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<String>>()
            .join(",");

        let pod_api: Api<Pod> = get_k8s_resource_api(client, namespace);
        let pods = pod_api
            .list(&ListParams::default().labels(&formatted_deployments_labels))
            .await
            .map_err(KubeApiError::KubeError)?;

        let first_pod = pods.items.first().ok_or_else(|| {
            KubeApiError::DeploymentNotFound(format!(
                "Failed to fetch the default(first pod) from ObjectList<Pod> for {}",
                self.target()
            ))
        })?;

        RuntimeData::from_pod(first_pod, self.container())
    }
}

impl RuntimeDataProvider for Target {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        match self {
            Target::Deployment(deployment) => deployment.runtime_data(client, namespace).await,
            Target::Pod(pod) => pod.runtime_data(client, namespace).await,
            Target::Rollout(rollout) => rollout.runtime_data(client, namespace).await,
            Target::Targetless => {
                unreachable!("runtime_data can't be called on Targetless")
            }
        }
    }
}

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
    fn target_parses(#[case] target: &str, #[case] expected: Target) {
        let target = target.parse::<Target>().unwrap();
        assert_eq!(target, expected)
    }

    #[rstest]
    #[should_panic(expected = "InvalidTarget")]
    #[case::panic("deployment/foobaz/blah")]
    #[should_panic(expected = "InvalidTarget")]
    #[case::panic("pod/foo/baz")]
    fn target_parse_fails(#[case] target: &str) {
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

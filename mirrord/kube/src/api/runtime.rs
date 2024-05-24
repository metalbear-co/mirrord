use std::{
    collections::BTreeMap,
    convert::Infallible,
    fmt::{self, Display, Formatter},
    ops::FromResidual,
    str::FromStr,
};

use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Node, Pod},
    },
    NamespaceResourceScope,
};
use kube::{api::ListParams, Api, Client, Resource};
use mirrord_config::target::{DeploymentTarget, PodTarget, RolloutTarget, Target};
use mirrord_protocol::MeshVendor;
use serde::de::DeserializeOwned;
use thiserror::Error;

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

#[derive(Error, Debug)]
#[error("invalid container runtime name: {0}")]
pub struct ContainerRuntimeParseError(String);

impl FromStr for ContainerRuntime {
    type Err = ContainerRuntimeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "docker" => Ok(Self::Docker),
            "containerd" => Ok(Self::Containerd),
            "cri-o" => Ok(Self::CriO),
            _ => Err(ContainerRuntimeParseError(s.to_string())),
        }
    }
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
    /// Extracts data needed to create the mirrord-agent targeting the given [`Pod`].
    /// Verifies that the [`Pod`] is ready to be a target:
    /// 1. pod is in "Running" phase,
    /// 2. pod is not in deletion,
    /// 3. target container is ready.
    pub fn from_pod(pod: &Pod, container_name: Option<&str>) -> Result<Self> {
        let pod_name = pod
            .metadata
            .name
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(pod, ".metadata.name"))?
            .to_owned();

        let phase = pod
            .status
            .as_ref()
            .and_then(|status| status.phase.as_ref())
            .ok_or_else(|| KubeApiError::missing_field(pod, ".status.phase"))?;
        if phase != "Running" {
            return Err(KubeApiError::invalid_state(pod, "not in 'Running' phase"));
        }

        if pod.metadata.deletion_timestamp.is_some() {
            return Err(KubeApiError::invalid_state(pod, "in deletion"));
        }

        let node_name = pod
            .spec
            .as_ref()
            .and_then(|spec| spec.node_name.as_ref())
            .ok_or_else(|| KubeApiError::missing_field(pod, ".spec.nodeName"))?
            .to_owned();

        let container_statuses = pod
            .status
            .as_ref()
            .and_then(|status| status.container_statuses.as_ref())
            .ok_or_else(|| KubeApiError::missing_field(pod, ".status.containerStatuses"))?;

        let (chosen_container, mesh) =
            choose_container(container_name, container_statuses.as_ref());

        let chosen_status = chosen_container.ok_or_else(|| match container_name {
            Some(name) => KubeApiError::invalid_state(
                pod,
                format_args!("target container `{name}` not found"),
            ),
            None => KubeApiError::invalid_state(pod, "no viable target container found"),
        })?;

        if !chosen_status.ready {
            return Err(KubeApiError::invalid_state(
                pod,
                format_args!("target container `{}` is not ready", chosen_status.name),
            ));
        }

        let container_name = chosen_status.name.clone();
        let container_id_full = chosen_status.container_id.as_ref().ok_or_else(|| {
            KubeApiError::missing_field(pod, ".status.containerStatuses.[].containerID")
        })?;

        let mut split = container_id_full.split("://");

        let (container_runtime, container_id) = match (
            split.next().map(ContainerRuntime::from_str),
            split.next(),
        ) {
            (Some(Ok(runtime)), Some(id)) => (runtime, id.to_string()),
            _ => {
                return Err(KubeApiError::invalid_value(
                    pod,
                    ".status.containerStatuses.[].containerID",
                    format_args!("failed to extract container runtime for `{container_name}`: `{container_id_full}`"),
                ));
            }
        };

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

        let allowed = node
            .status
            .as_ref()
            .and_then(|status| status.allocatable.as_ref())
            .and_then(|allocatable| allocatable.get("pods"))
            .ok_or_else(|| KubeApiError::missing_field(&node, ".status.allocatable.pods"))?
            .0
            .parse::<usize>()
            .map_err(|e| KubeApiError::invalid_value(&node, ".status.allocatable.pods", e))?;

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
            NodeCheck::Failed(node, pod_count)
        } else {
            NodeCheck::Success
        }
    }
}

#[derive(Debug)]
pub enum NodeCheck {
    Success,
    Failed(Node, usize),
    Error(KubeApiError),
}

impl<E> FromResidual<Result<Infallible, E>> for NodeCheck
where
    E: Into<KubeApiError>,
{
    fn from_residual(error: Result<Infallible, E>) -> Self {
        match error {
            Ok(_) => unreachable!(),
            Err(err) => NodeCheck::Error(err.into()),
        }
    }
}

pub trait RuntimeDataProvider {
    #[allow(async_fn_in_trait)]
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData>;
}

pub trait RuntimeDataFromLabels {
    type Resource: Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Clone
        + DeserializeOwned
        + fmt::Debug;

    #[allow(async_fn_in_trait)]
    async fn get_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>>;

    fn name(&self) -> &str;

    fn container(&self) -> Option<&str>;
}

impl<T> RuntimeDataProvider for T
where
    T: RuntimeDataFromLabels,
{
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let api: Api<<Self as RuntimeDataFromLabels>::Resource> =
            get_k8s_resource_api(client, namespace);
        let resource = api.get(self.name()).await?;

        let labels = Self::get_labels(&resource).await?;

        let formatted_labels = labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<String>>()
            .join(",");
        let list_params = ListParams {
            label_selector: Some(formatted_labels),
            ..Default::default()
        };

        let pod_api: Api<Pod> = get_k8s_resource_api(client, namespace);
        let pods = pod_api.list(&list_params).await?;

        if pods.items.is_empty() {
            return Err(KubeApiError::invalid_state(
                &resource,
                "no pods matching labels found",
            ));
        }

        pods.items
            .iter()
            .filter_map(|pod| RuntimeData::from_pod(pod, self.container()).ok())
            .next()
            .ok_or_else(|| {
                KubeApiError::invalid_state(
                    &resource,
                    "no pod matching labels is ready to be targeted",
                )
            })
    }
}

impl RuntimeDataProvider for Target {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        match self {
            Target::Deployment(deployment) => deployment.runtime_data(client, namespace).await,
            Target::Pod(pod) => pod.runtime_data(client, namespace).await,
            Target::Rollout(rollout) => rollout.runtime_data(client, namespace).await,
            Target::Targetless => Err(KubeApiError::MissingRuntimeData),
        }
    }
}

impl RuntimeDataFromLabels for DeploymentTarget {
    type Resource = Deployment;

    fn name(&self) -> &str {
        &self.deployment
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    async fn get_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.match_labels.clone())
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

impl RuntimeDataFromLabels for RolloutTarget {
    type Resource = Rollout;

    fn name(&self) -> &str {
        &self.rollout
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    async fn get_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        resource
            .match_labels()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.selector.matchLabels"))
    }
}

impl RuntimeDataProvider for PodTarget {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let pod_api: Api<Pod> = get_k8s_resource_api(client, namespace);
        let pod = pod_api.get(&self.pod).await?;

        RuntimeData::from_pod(&pod, self.container.as_deref())
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

    #[allow(clippy::duplicated_attributes)]
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

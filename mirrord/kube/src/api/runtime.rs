use std::{
    borrow::Cow,
    fmt::{self, Display, Formatter},
    future::Future,
    net::IpAddr,
    ops::Not,
    str::FromStr,
};

use k8s_openapi::{
    NamespaceResourceScope,
    api::core::v1::{Container, ContainerPort, Node, Pod, PodStatus, Probe},
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{Api, Client, Resource, api::ListParams, core::Selector};
use mirrord_agent_env::mesh::MeshVendor;
use mirrord_config::target::Target;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::Level;

use crate::{
    api::{
        container::{check_mesh_vendor, choose_container},
        kubernetes::get_k8s_resource_api,
    },
    error::{KubeApiError, Result},
    extract::{
        FromResource,
        metadata::{Name, Namespace},
    },
    resolved::ResolvedTarget,
};

pub mod cron_job;
pub mod deployment;
pub mod job;
pub mod label;
pub mod pod;
pub mod replica_set;
pub mod rollout;
pub mod service;
pub mod stateful_set;

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
            _ => Err(ContainerRuntimeParseError(s.to_owned())),
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
    pub pod_ips: Vec<IpAddr>,
    pub host_ips: Vec<IpAddr>,
    pub pod_namespace: String,
    pub node_name: String,
    pub node_hostname: Option<String>,
    pub container_id: String,
    pub container_runtime: ContainerRuntime,
    pub container_name: String,
    /// True when no container was specified by the user, but there are multiple containers,
    /// so mirrord chose one of them for the user.
    pub guessed_container: bool,

    /// Used to check if we're running with a mesh/sidecar in `detect_mesh_mirror_mode`.
    pub mesh: Option<MeshVendor>,

    pub share_process_namespace: bool,

    /// Ports where HTTP/gRPC probes are configured
    /// in the target pod.
    pub containers_probe_ports: Vec<u16>,

    /// Ports declared in `.spec.containers[].ports` of the target container.
    ///
    /// Kubernetes does not require containers to declare their ports, so an empty list means that
    /// we do not know which ports the target container listens on.
    pub container_ports: Vec<u16>,
}

impl RuntimeData {
    /// Standard annotation containing the name of the [`Pod`]'s default container.
    ///
    /// See [Kubernetes docs](https://kubernetes.io/docs/reference/labels-annotations-taints/#kubectl-kubernetes-io-default-container).
    pub const DEFAULT_CONTAINER_ANNOTATION: &str = "kubectl.kubernetes.io/default-container";

    /// Extracts data needed to create the mirrord-agent targeting the given [`Pod`].
    /// Verifies that the [`Pod`] is ready to be a target:
    /// 1. pod is in "Running" phase,
    /// 2. pod is not in deletion,
    /// 3. target container is ready.
    pub fn from_pod(pod: &Pod, container_name: Option<&str>) -> Result<Self> {
        let (Name(pod_name), Namespace(pod_namespace)) = FromResource::from_resource(pod, &())?;

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

        let containers_probe_ports: Vec<u16> = pod
            .spec
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(pod, ".spec"))?
            .containers
            .iter()
            .flat_map(Self::probe_ports)
            .collect();

        let pod_ips = pod
            .status
            .as_ref()
            .and_then(|spec| spec.pod_ips.as_ref())
            .ok_or_else(|| KubeApiError::missing_field(pod, ".status.podIPs"))?
            .iter()
            .filter_map(|pod_ip| {
                pod_ip
                    .ip
                    .parse::<IpAddr>()
                    .inspect_err(|e| {
                        tracing::warn!("failed to parse pod IP {ip}: {e:#?}", ip = pod_ip.ip);
                    })
                    .ok()
            })
            .collect();

        let host_ips = Self::host_ips(
            pod.status
                .as_ref()
                .ok_or_else(|| KubeApiError::missing_field(pod, ".status"))?,
        );

        let container_statuses = pod
            .status
            .as_ref()
            .and_then(|status| status.container_statuses.as_ref())
            .ok_or_else(|| KubeApiError::missing_field(pod, ".status.containerStatuses"))?;
        let default_container_name = pod
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(Self::DEFAULT_CONTAINER_ANNOTATION))
            .map(String::as_str);

        let (chosen_container, guessed_container) = choose_container(
            container_name,
            default_container_name,
            container_statuses.as_ref(),
        );

        if guessed_container {
            tracing::warn!("mirrord picked first eligible container out of many");
        }

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

        let container_ports = pod
            .spec
            .as_ref()
            .and_then(|spec| {
                spec.containers
                    .iter()
                    .find(|container| container.name == container_name)
            })
            .and_then(|container| container.ports.as_deref())
            .unwrap_or_default()
            .iter()
            // mirrord subscribes only to TCP ports, and Kubernetes uses TCP when no protocol is
            // set.
            .filter(|port| {
                port.protocol
                    .as_deref()
                    .is_none_or(|protocol| protocol == "TCP")
            })
            .filter_map(|port| u16::try_from(port.container_port).ok())
            .collect();

        let container_id_full = chosen_status.container_id.as_ref().ok_or_else(|| {
            KubeApiError::missing_field(pod, ".status.containerStatuses.[].containerID")
        })?;

        let mut split = container_id_full.split("://");

        let (container_runtime, container_id) = match (
            split.next().map(ContainerRuntime::from_str),
            split.next(),
        ) {
            (Some(Ok(runtime)), Some(id)) => (runtime, id.to_owned()),
            _ => {
                return Err(KubeApiError::invalid_value(
                    pod,
                    ".status.containerStatuses.[].containerID",
                    format_args!(
                        "failed to extract container runtime for `{container_name}`: `{container_id_full}`"
                    ),
                ));
            }
        };

        let mesh = check_mesh_vendor(pod);

        Ok(RuntimeData {
            pod_ips,
            host_ips,
            pod_name: pod_name.to_owned(),
            pod_namespace: pod_namespace.to_owned(),
            node_name,
            node_hostname: None,
            container_id,
            container_runtime,
            container_name,
            guessed_container,
            mesh,
            share_process_namespace: pod
                .spec
                .as_ref()
                .and_then(|spec| spec.share_process_namespace)
                .unwrap_or_default(),
            containers_probe_ports,
            container_ports,
        })
    }

    fn host_ips(status: &PodStatus) -> Vec<IpAddr> {
        let mut parsed = Vec::new();

        if let Some(host_ip) = status.host_ip.as_deref() {
            Self::push_host_ip(&mut parsed, host_ip);
        }

        if let Some(host_ips) = status.host_ips.as_deref() {
            for host_ip in host_ips {
                Self::push_host_ip(&mut parsed, &host_ip.ip);
            }
        }

        parsed
    }

    fn push_host_ip(host_ips: &mut Vec<IpAddr>, host_ip: &str) {
        match host_ip.parse() {
            Ok(host_ip) if host_ips.contains(&host_ip).not() => host_ips.push(host_ip),
            Ok(..) => {}
            Err(error) => {
                tracing::warn!(%host_ip, ?error, "failed to parse host IP");
            }
        }
    }

    /// Resolves and stores `kubernetes.io/hostname` label from the target node when available.
    ///
    /// In vcluster, target pods are scheduled on fake nodes in the virtual cluster. These nodes
    /// describe where the workload runs from the virtual cluster's perspective, but their
    /// `kubernetes.io/hostname` label is not a valid selector for scheduling a real pod in the
    /// host cluster. vcluster marks them with `vcluster.loft.sh/fake-node=true`; when we see that
    /// marker, we intentionally ignore the hostname label and let the agent scheduler use the node
    /// name extracted from the target pod instead.
    ///
    /// This is best-effort and intentionally non-fatal.
    #[tracing::instrument(level = Level::TRACE, skip(client))]
    pub async fn try_resolve_node_hostname(&mut self, client: &Client) {
        const NODE_HOSTNAME_LABEL: &str = "kubernetes.io/hostname";
        const VCLUSTER_FAKE_NODE_LABEL: &str = "vcluster.loft.sh/fake-node";

        if self.node_hostname.is_some() {
            return;
        }

        let node_api: Api<Node> = Api::all(client.clone());

        let resolved = node_api
            .get_metadata(&self.node_name)
            .await
            .ok()
            .and_then(|node| node.metadata.labels)
            .and_then(|mut labels| {
                if labels
                    .get(VCLUSTER_FAKE_NODE_LABEL)
                    .is_some_and(|value| value == "true")
                {
                    None
                } else {
                    labels.remove(NODE_HOSTNAME_LABEL)
                }
            });

        self.node_hostname = resolved;
    }

    #[tracing::instrument(level = Level::TRACE, skip(client), ret)]
    pub async fn check_node(&self, client: &kube::Client) -> NodeCheck {
        match self.check_node_inner(client).await {
            Ok(check) => check,
            Err(error) => NodeCheck::Error(error),
        }
    }

    async fn check_node_inner(&self, client: &kube::Client) -> Result<NodeCheck, KubeApiError> {
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
            field_selector: Some(format!(
                "status.phase=Running,spec.nodeName={}",
                self.node_name
            )),
            ..Default::default()
        };

        loop {
            let pods_on_node = pod_api.list_metadata(&list_params).await?;

            pod_count += pods_on_node.items.len();

            match pods_on_node.metadata.continue_ {
                Some(next) if next.is_empty().not() => {
                    list_params = list_params.continue_token(&next);
                }
                None | Some(..) => break,
            }
        }

        if allowed <= pod_count {
            Ok(NodeCheck::Failed {
                node: Box::new(node),
                pod_count,
            })
        } else {
            Ok(NodeCheck::Success)
        }
    }

    /// Returns a list of ports used by the container's readiness and liveness probes.
    ///
    /// * Startup probes are ignored: target needs to be ready to start a new session anyway
    /// * TCP probes are ignored: the agent will accept the connection anyway
    fn probe_ports(container: &Container) -> Vec<u16> {
        fn port_to_int(port: &IntOrString, container_ports: &[ContainerPort]) -> Option<u16> {
            match port {
                IntOrString::Int(p) => Some(*p as u16),
                IntOrString::String(port_name) => container_ports
                    .iter()
                    .find(|p| p.name.as_ref() == Some(port_name))
                    .and_then(|p| p.container_port.try_into().ok()),
            }
        }

        fn get_probe_ports(probe: &Probe, container_ports: &[ContainerPort]) -> Vec<u16> {
            let mut ports = Vec::new();

            if let Some(port) = probe
                .http_get
                .as_ref()
                .and_then(|get| port_to_int(&get.port, container_ports))
            {
                ports.push(port);
            }

            if let Some(port) = probe
                .grpc
                .as_ref()
                .and_then(|grpc| grpc.port.try_into().ok())
            {
                ports.push(port);
            }

            ports
        }

        let mut ports = Vec::new();

        if let Some(liveness) = &container.liveness_probe {
            ports.extend(get_probe_ports(
                liveness,
                container.ports.as_deref().unwrap_or_default(),
            ));
        }

        if let Some(readiness) = &container.readiness_probe {
            ports.extend(get_probe_ports(
                readiness,
                container.ports.as_deref().unwrap_or_default(),
            ));
        }

        ports
    }
}

#[derive(Debug)]
pub enum NodeCheck {
    Success,
    Failed {
        /// [`Node`] fetched from the API.
        ///
        /// Boxed due to large size (>1kb).
        node: Box<Node>,
        /// Count of pods found on the node.
        pod_count: usize,
    },
    Error(KubeApiError),
}

pub trait RuntimeDataProvider {
    fn runtime_data(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> impl Future<Output = Result<RuntimeData>>;
}

/// Accepts label maps or nonempty expressions, including expression-only workloads.
/// Explicitly empty `matchLabels` maps preserve unrestricted workload selectors;
/// rejecting them would make those targets fail with missing-selector errors.
pub(crate) fn has_match_labels_or_expressions(selector: &LabelSelector) -> bool {
    selector.match_labels.is_some()
        || selector
            .match_expressions
            .as_ref()
            .is_some_and(|expressions| expressions.is_empty().not())
}

/// Trait for resources that abstract a set of pods
/// defined by a label selector, including both label equalities and expressions.
///
/// Implementors are provided with an implementation of [`RuntimeDataProvider`].
/// When resolving [`RuntimeData`], the set of pods is fetched and [`RuntimeData`] is extracted from
/// the first pod ready to be targeted. If the set is empty or no pod is ready, resolution fails.
pub trait RuntimeDataFromLabels {
    type Resource: Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Clone
        + DeserializeOwned
        + fmt::Debug;

    fn get_selector(resource: &Self::Resource) -> Result<Selector>;

    /// Returns a list of pods matching the selector of the given `resource`.
    fn get_pods(
        resource: &Self::Resource,
        client: &Client,
    ) -> impl Future<Output = Result<Vec<Pod>>> {
        async {
            let api: Api<<Self as RuntimeDataFromLabels>::Resource> =
                get_k8s_resource_api(client, resource.meta().namespace.as_deref());
            let name = resource
                .meta()
                .name
                .as_deref()
                .ok_or_else(|| KubeApiError::missing_field(resource, ".metadata.name"))?;
            let resource = api.get(name).await?;

            let selector = Self::get_selector(&resource)?;
            let list_params = ListParams::default().labels_from(&selector);

            let pod_api: Api<Pod> =
                get_k8s_resource_api(client, resource.meta().namespace.as_deref());
            let pods = pod_api.list(&list_params).await?;

            Ok(pods.items)
        }
    }

    fn name(&self) -> Cow<'_, str>;

    fn container(&self) -> Option<&str>;
}

impl<T> RuntimeDataProvider for T
where
    T: RuntimeDataFromLabels,
{
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let api: Api<<Self as RuntimeDataFromLabels>::Resource> =
            get_k8s_resource_api(client, namespace);
        let resource = api.get(&self.name()).await?;
        let pods = Self::get_pods(&resource, client).await?;

        if pods.is_empty() {
            return Err(KubeApiError::invalid_state(
                &resource,
                "no pods matching the labels were found",
            ));
        }

        pods.iter()
            .filter_map(|pod| RuntimeData::from_pod(pod, self.container()).ok())
            .next()
            .ok_or_else(|| {
                KubeApiError::invalid_state(
                    &resource,
                    "no pod matching the labels is ready to be targeted",
                )
            })
    }
}

impl RuntimeDataProvider for Target {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        match self {
            Target::Deployment(deployment) => deployment.runtime_data(client, namespace).await,
            Target::Pod(target) => target.runtime_data(client, namespace).await,
            Target::Rollout(target) => target.runtime_data(client, namespace).await,
            Target::Job(target) => target.runtime_data(client, namespace).await,
            Target::CronJob(target) => target.runtime_data(client, namespace).await,
            Target::StatefulSet(target) => target.runtime_data(client, namespace).await,
            Target::Service(target) => target.runtime_data(client, namespace).await,
            Target::ReplicaSet(target) => target.runtime_data(client, namespace).await,
            Target::Label(target) => target.runtime_data(client, namespace).await,
            Target::Targetless | Target::Serverless(_) => Err(KubeApiError::MissingRuntimeData),
        }
    }
}

impl RuntimeDataProvider for ResolvedTarget<true> {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        match self {
            Self::Deployment(target) => target.runtime_data(client, namespace).await,
            Self::Pod(target) => target.runtime_data().await,
            Self::Rollout(target) => target.runtime_data(client, namespace).await,
            Self::Job(target) => target.runtime_data(client, namespace).await,
            Self::CronJob(target) => target.runtime_data(client, namespace).await,
            Self::StatefulSet(target) => target.runtime_data(client, namespace).await,
            Self::Service(target) => target.runtime_data(client, namespace).await,
            Self::ReplicaSet(target) => target.runtime_data(client, namespace).await,
            Self::Label(target) => target.representative.runtime_data().await,
            Self::Targetless(_) => Err(KubeApiError::MissingRuntimeData),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, Ipv6Addr},
        time::Duration,
    };

    use http::{Method, Request, Response};
    use k8s_openapi::api::{apps::v1::Deployment, core::v1::HostIP};
    use kube::client::Body;
    use mirrord_config::target::{
        deployment::DeploymentTarget, job::JobTarget, pod::PodTarget, rollout::RolloutTarget,
        service::ServiceTarget,
    };
    use rstest::rstest;

    use super::*;
    use crate::{api::kubernetes::rollout::Rollout, resolved::ResolvedResource};

    fn assert_pod_list_selector(request: &Request<Body>, expected_selector: &str) {
        assert_eq!(request.method(), Method::GET);
        assert_eq!(request.uri().path(), "/api/v1/namespaces/workloads/pods");
        let selectors: Vec<_> = request
            .uri()
            .query()
            .unwrap_or_default()
            .split('&')
            .filter_map(|parameter| parameter.strip_prefix("labelSelector="))
            .collect();
        assert_eq!(selectors.len(), 1);
        let mut parts = selectors.first().unwrap().split('%');
        let mut decoded_selector = parts.next().unwrap().replace('+', " ");
        for part in parts {
            let (byte, rest) = part.split_at(2);
            decoded_selector.push(char::from(u8::from_str_radix(byte, 16).unwrap()));
            decoded_selector.push_str(&rest.replace('+', " "));
        }
        assert_eq!(decoded_selector, expected_selector);
    }

    fn pod_list_response() -> Response<Body> {
        Response::new(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": [],
            }))
            .unwrap(),
        ))
    }

    #[rstest]
    #[case::mixed(serde_json::json!({"matchLabels": {"app": "api"}, "matchExpressions": [{"key": "tier", "operator": "In", "values": ["backend", "worker"]}]}), Some("app=api,tier in (backend,worker)"))]
    #[case::expressions_only(serde_json::json!({"matchExpressions": [{"key": "tier", "operator": "In", "values": ["backend", "worker"]}]}), Some("tier in (backend,worker)"))]
    #[case::empty_object(serde_json::json!({}), None)]
    #[case::empty_expressions(serde_json::json!({"matchExpressions": []}), None)]
    #[case::empty_labels(serde_json::json!({"matchLabels": {}}), Some(""))]
    #[tokio::test]
    async fn deployment_pod_list_selector(
        #[case] selector: serde_json::Value,
        #[case] expected_selector: Option<&str>,
        #[values(false, true)] resolved: bool,
    ) {
        let deployment: Deployment = serde_json::from_value(serde_json::json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": "api", "namespace": "workloads"},
            "spec": {"selector": selector, "template": {}},
        }))
        .unwrap();
        let (service, mut handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let client = Client::new(service, "default");

        tokio::time::timeout(Duration::from_secs(5), async {
            let (result, ()) = tokio::join!(
                async {
                    if resolved {
                        ResolvedResource::<Deployment>::get_pods(&deployment, &client).await
                    } else {
                        DeploymentTarget::get_pods(&deployment, &client).await
                    }
                },
                async {
                    let (request, send) = handle.next_request().await.unwrap();
                    assert_eq!(request.method(), Method::GET);
                    assert_eq!(
                        request.uri().path(),
                        "/apis/apps/v1/namespaces/workloads/deployments/api"
                    );
                    send.send_response(Response::new(Body::from(
                        serde_json::to_vec(&deployment).unwrap(),
                    )));

                    if let Some(expected_selector) = expected_selector {
                        let (request, send) = handle.next_request().await.unwrap();
                        assert_pod_list_selector(&request, expected_selector);
                        send.send_response(pod_list_response());
                    }
                },
            );
            if expected_selector.is_some() {
                result.unwrap();
            } else {
                assert!(
                    matches!(result, Err(KubeApiError::MalformedResource(ref message))
                    if message.contains(".spec.selector.matchLabels"))
                );
            }
        })
        .await
        .expect("deployment pod-list request timed out");
    }

    #[rstest]
    #[case::mixed(Some(serde_json::json!({"app": "api"})), "app=api,tier in (backend,worker)")]
    #[case::expressions_only(None, "tier in (backend,worker)")]
    #[tokio::test]
    async fn rollout_pod_list_preserves_selector_expressions(
        #[case] match_labels: Option<serde_json::Value>,
        #[case] expected_selector: &str,
        #[values(false, true)] referenced: bool,
    ) {
        let mut selector = serde_json::json!({
            "matchExpressions": [{"key": "tier", "operator": "In", "values": ["backend", "worker"]}],
        });
        if let Some(match_labels) = match_labels {
            selector
                .as_object_mut()
                .unwrap()
                .insert("matchLabels".to_owned(), match_labels);
        }
        let rollout: Rollout = serde_json::from_value(serde_json::json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Rollout",
            "metadata": {"name": "api", "namespace": "workloads"},
            "spec": if referenced {
                serde_json::json!({"workloadRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "api"}})
            } else {
                serde_json::json!({"selector": selector})
            },
        })).unwrap();
        let (service, mut handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let client = Client::new(service, "default");

        tokio::time::timeout(Duration::from_secs(5), async {
            let (_pods, ()) = tokio::try_join!(RolloutTarget::get_pods(&rollout, &client), async {
                if referenced {
                    let (request, send) = handle.next_request().await.unwrap();
                    assert_eq!(request.method(), Method::GET);
                    assert_eq!(
                        request.uri().path(),
                        "/apis/apps/v1/namespaces/workloads/deployments/api"
                    );
                    send.send_response(Response::new(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "apiVersion": "apps/v1", "kind": "Deployment",
                            "metadata": {"name": "api", "namespace": "workloads"},
                            "spec": {"selector": selector, "template": {}},
                        }))
                        .unwrap(),
                    )));
                }
                let (request, send) = handle.next_request().await.unwrap();
                assert_pod_list_selector(&request, expected_selector);
                send.send_response(pod_list_response());
                Ok(())
            })
            .unwrap();
        })
        .await
        .expect("rollout pod-list request timed out");
    }

    #[test]
    fn host_ips_preserve_kubernetes_order_and_remove_duplicates() {
        let status = PodStatus {
            host_ip: Some("10.0.0.1".to_owned()),
            host_ips: Some(vec![
                HostIP {
                    ip: "10.0.0.1".to_owned(),
                },
                HostIP {
                    ip: "2001:db8::1".to_owned(),
                },
                HostIP {
                    ip: "10.0.0.1".to_owned(),
                },
            ]),
            ..Default::default()
        };

        assert_eq!(
            RuntimeData::host_ips(&status),
            vec![
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            ]
        );
    }

    #[test]
    fn host_ips_fall_back_to_singular_host_ip_when_list_is_empty() {
        let status = PodStatus {
            host_ip: Some("10.0.0.1".to_owned()),
            host_ips: Some(Vec::new()),
            ..Default::default()
        };

        assert_eq!(
            RuntimeData::host_ips(&status),
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]
        );
    }

    #[test]
    fn host_ips_ignore_malformed_addresses() {
        let status = PodStatus {
            host_ips: Some(vec![HostIP {
                ip: "not-an-ip".to_owned(),
            }]),
            ..Default::default()
        };

        assert_eq!(RuntimeData::host_ips(&status), Vec::<IpAddr>::new());
    }

    #[test]
    fn host_ips_keep_singular_host_ip_when_list_is_malformed() {
        let status = PodStatus {
            host_ip: Some("10.0.0.1".to_owned()),
            host_ips: Some(vec![HostIP {
                ip: "not-an-ip".to_owned(),
            }]),
            ..Default::default()
        };

        assert_eq!(
            RuntimeData::host_ips(&status),
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]
        );
    }

    /// [`RuntimeData::container_ports`] must hold only the TCP ports of the target container, and
    /// not the ports of other containers in the pod.
    #[rstest]
    #[case(Some("app"), vec![8080, 9090, 9091])]
    #[case(Some("sidecar"), vec![15001])]
    #[case(Some("no-ports"), vec![])]
    fn container_ports_come_from_target_container(
        #[case] container_name: Option<&str>,
        #[case] expected: Vec<u16>,
    ) {
        let container_statuses = ["app", "sidecar", "no-ports"].map(|name| {
            serde_json::json!({
                "name": name,
                "ready": true,
                "containerID": format!("containerd://{name}"),
                "image": "image",
                "imageID": "image",
                "restartCount": 0,
            })
        });

        let pod: Pod = serde_json::from_value(serde_json::json!({
            "metadata": { "name": "pod", "namespace": "default" },
            "spec": {
                "nodeName": "node",
                "containers": [
                    {
                        "name": "app",
                        "ports": [
                            { "containerPort": 8080 },
                            { "containerPort": 9090, "protocol": "TCP" },
                            { "containerPort": 9091, "protocol": "TCP" },
                            { "containerPort": 9091, "protocol": "UDP" },
                            { "containerPort": 5353, "protocol": "UDP" },
                        ],
                    },
                    { "name": "sidecar", "ports": [{ "containerPort": 15001 }] },
                    { "name": "no-ports" },
                ],
            },
            "status": {
                "phase": "Running",
                "podIPs": [{ "ip": "10.0.0.2" }],
                "containerStatuses": container_statuses,
            },
        }))
        .unwrap();

        let runtime_data = RuntimeData::from_pod(&pod, container_name).unwrap();

        assert_eq!(runtime_data.container_ports, expected);
    }

    #[rstest]
    #[case("pod/foobaz", Target::Pod(PodTarget {pod: "foobaz".to_owned(), container: None}))]
    #[case("deployment/foobaz", Target::Deployment(DeploymentTarget {deployment: "foobaz".to_owned(), container: None}))]
    #[case("deployment/nginx-deployment", Target::Deployment(DeploymentTarget {deployment: "nginx-deployment".to_owned(), container: None}))]
    #[case("pod/foo/container/baz", Target::Pod(PodTarget { pod: "foo".to_owned(), container: Some("baz".to_owned()) }))]
    #[case("deployment/nginx-deployment/container/container-name", Target::Deployment(DeploymentTarget {deployment: "nginx-deployment".to_owned(), container: Some("container-name".to_owned())}))]
    #[case("job/foo", Target::Job(JobTarget { job: "foo".to_owned(), container: None }))]
    #[case("job/foo/container/baz", Target::Job(JobTarget { job: "foo".to_owned(), container: Some("baz".to_owned()) }))]
    #[case("service/foo", Target::Service(ServiceTarget { service: "foo".into(), container: None }))]
    #[case("service/foo/container/baz", Target::Service(ServiceTarget { service: "foo".into(), container: Some("baz".into()) }))]
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
                deployment: "foobaz".to_owned(),
                container: None
            })
        )
    }
}

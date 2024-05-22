use std::collections::HashMap;

use bollard::{container::InspectContainerOptions, Docker, API_DEFAULT_VERSION};
use containerd_client::{
    services::v1::{
        containers_client::ContainersClient, tasks_client::TasksClient, GetContainerRequest,
        GetRequest,
    },
    tonic::{transport::Channel, Request},
    with_namespace,
};
use enum_dispatch::enum_dispatch;
use oci_spec::runtime::Spec;
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;

use crate::{
    env::parse_raw_env,
    error::{AgentError, Result},
    runtime::crio::CriOContainer,
};

mod crio;

const CONTAINERD_DEFAULT_SOCK_PATH: &str = "/host/run/containerd/containerd.sock";
const CONTAINERD_ALTERNATIVE_SOCK_PATH: &str = "/host/run/dockershim.sock";
const CONTAINERD_K3S_SOCK_PATH: &str = "/host/run/k3s/containerd/containerd.sock";
const CONTAINERD_MICROK8S_SOCK_PATH: &str = "/host/var/snap/microk8s/common/run/containerd.sock";
const CONTAINERD_K0S_SOCK_PATH: &str = "/host/run/k0s/containerd.sock";

/// Possible containerd socket paths, evaluated from left to right.
const CONTAINERD_SOCK_PATHS: [&str; 5] = [
    CONTAINERD_DEFAULT_SOCK_PATH,
    CONTAINERD_ALTERNATIVE_SOCK_PATH,
    CONTAINERD_K3S_SOCK_PATH,
    CONTAINERD_MICROK8S_SOCK_PATH,
    CONTAINERD_K0S_SOCK_PATH,
];

const DEFAULT_CONTAINERD_NAMESPACE: &str = "k8s.io";

#[derive(Debug)]
pub(crate) struct ContainerInfo {
    /// External PID of the container
    pub(crate) pid: u64,
    /// Environment variables of the container
    pub(crate) env: HashMap<String, String>,
}

impl ContainerInfo {
    pub(crate) fn new(pid: u64, env: HashMap<String, String>) -> Self {
        ContainerInfo { pid, env }
    }
}

#[enum_dispatch]
pub(crate) trait ContainerRuntime {
    /// Get information about the container (pid, env).
    async fn get_info(&self) -> Result<ContainerInfo>;
}

#[enum_dispatch(ContainerRuntime)]
#[derive(Debug, Clone)]
pub(crate) enum Container {
    Docker(DockerContainer),
    Containerd(ContainerdContainer),
    CriO(CriOContainer),
    Ephemeral(EphemeralContainer),
}

/// get a container object according to args.
pub(crate) async fn get_container(
    container_id: String,
    container_runtime: Option<&str>,
) -> Result<Container> {
    match container_runtime {
        Some("docker") => Ok(Container::Docker(
            DockerContainer::from_id(container_id).await?,
        )),
        Some("containerd") => Ok(Container::Containerd(ContainerdContainer { container_id })),
        Some("cri-o") => Ok(Container::CriO(CriOContainer::from_id(container_id))),
        _ => Err(AgentError::NotFound(format!(
            "Unknown runtime {container_runtime:?}"
        ))),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DockerContainer {
    container_id: String,
    client: Docker,
}

impl DockerContainer {
    async fn from_id(container_id: String) -> Result<Self> {
        let client = match Docker::connect_with_unix(
            "unix:///host/run/docker.sock",
            10,
            API_DEFAULT_VERSION,
        ) {
            Ok(client) if client.ping().await.is_ok() => client,
            _ => Docker::connect_with_unix(
                "unix:///host/var/run/docker.sock",
                10,
                API_DEFAULT_VERSION,
            )?,
        };

        Ok(DockerContainer {
            container_id,
            client,
        })
    }
}

impl ContainerRuntime for DockerContainer {
    async fn get_info(&self) -> Result<ContainerInfo> {
        let inspect_options = Some(InspectContainerOptions { size: false });
        let inspect_response = self
            .client
            .inspect_container(&self.container_id, inspect_options)
            .await?;

        let pid = inspect_response
            .state
            .and_then(|state| state.pid)
            .and_then(|pid| if pid > 0 { Some(pid as u64) } else { None })
            .ok_or_else(|| AgentError::NotFound("No pid found!".to_string()))?;

        let raw_env = inspect_response
            .config
            .and_then(|config| config.env)
            .ok_or_else(|| AgentError::NotFound("No env found!".to_string()))?;
        let env_vars = parse_raw_env(&raw_env);

        Ok(ContainerInfo::new(pid, env_vars))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerdContainer {
    container_id: String,
}

async fn connect(path: impl AsRef<std::path::Path>) -> Result<Channel> {
    let path = path.as_ref().to_path_buf();

    Endpoint::try_from("http://localhost")?
        .connect_with_connector(service_fn(move |_: Uri| UnixStream::connect(path.clone())))
        .await
        .map_err(AgentError::from)
}

/// Connects to the given containerd socket
/// and returns the client only if the given container
/// exists.
async fn connect_and_find_container(
    container_id: String,
    sock_path: impl AsRef<std::path::Path>,
) -> Result<Channel> {
    let channel = connect(sock_path).await?;
    let mut client = TasksClient::new(channel.clone());
    let request = GetRequest {
        container_id,
        ..Default::default()
    };
    let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
    client.get(request).await?;
    Ok(channel)
}

/// Extract from [`Spec`] struct the environment variables as HashMap<K,V>
fn extract_env_from_containerd_spec(spec: &Spec) -> Option<HashMap<String, String>> {
    Some(parse_raw_env(spec.process().as_ref()?.env().as_ref()?))
}
impl ContainerdContainer {
    /// Get the containerd channel for a given container id.
    /// This is useful since we might have more than one
    /// containerd socket to use and we need to find the one
    /// that manages our target container
    async fn get_channel(&self) -> Result<Channel> {
        for sock_path in CONTAINERD_SOCK_PATHS {
            if let Ok(channel) =
                connect_and_find_container(self.container_id.clone(), sock_path).await
            {
                return Ok(channel);
            }
        }
        Err(AgentError::ContainerdSocketNotFound)
    }

    async fn get_task_client(&self) -> Result<TasksClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(TasksClient::new(channel))
    }

    async fn get_container_client(&self) -> Result<ContainersClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ContainersClient::new(channel))
    }
}

impl ContainerRuntime for ContainerdContainer {
    async fn get_info(&self) -> Result<ContainerInfo> {
        let mut client = self.get_task_client().await?;
        let container_id = self.container_id.to_string();
        let request = GetRequest {
            container_id,
            ..Default::default()
        };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
        let pid = client
            .get(request)
            .await?
            .into_inner()
            .process
            .ok_or_else(|| AgentError::NotFound("No pid found!".to_string()))?
            .pid;

        let mut client = self.get_container_client().await?;
        let request = GetContainerRequest {
            id: self.container_id.to_string(),
        };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);

        let spec: Spec = client
            .get(request)
            .await?
            .into_inner()
            .container
            .and_then(|c| c.spec)
            .ok_or_else(|| AgentError::NotFound("Spec wasn't found".to_string()))
            .map(|s| serde_json::from_slice(&s.value))??;

        let env_vars = extract_env_from_containerd_spec(&spec)
            .ok_or_else(|| AgentError::NotFound("No env vars found!".to_string()))?;

        Ok(ContainerInfo::new(pid as u64, env_vars))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EphemeralContainer;

impl ContainerRuntime for EphemeralContainer {
    /// When running on ephemeral, root pid is always 1 and env is the current process' env. (we
    /// copy it from the k8s spec)
    async fn get_info(&self) -> Result<ContainerInfo> {
        Ok(ContainerInfo::new(1, std::env::vars().collect()))
    }
}

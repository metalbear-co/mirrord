use std::{
    collections::HashMap,
    fs::File,
    os::unix::io::{IntoRawFd, RawFd},
    path::PathBuf,
};

use bollard::{container::InspectContainerOptions, Docker, API_DEFAULT_VERSION};
use containerd_client::{
    connect,
    services::v1::{
        containers_client::ContainersClient, tasks_client::TasksClient, GetContainerRequest,
        GetRequest, PauseTaskRequest, ResumeTaskRequest,
    },
    tonic::{transport::Channel, Request},
    with_namespace,
};
use enum_dispatch::enum_dispatch;
use nix::sched::setns;
use oci_spec::runtime::Spec;
use tracing::trace;

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

/// Possible containerd socket paths, evaluated from left to right.
const CONTAINERD_SOCK_PATHS: [&str; 4] = [
    CONTAINERD_DEFAULT_SOCK_PATH,
    CONTAINERD_ALTERNATIVE_SOCK_PATH,
    CONTAINERD_K3S_SOCK_PATH,
    CONTAINERD_MICROK8S_SOCK_PATH,
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
    /// Pause the whole container (all processes).
    async fn pause(&self) -> Result<()>;
    /// Unpause the whole container (all processes).
    async fn unpause(&self) -> Result<()>;
}

#[enum_dispatch(ContainerRuntime)]
#[derive(Debug)]
pub(crate) enum Container {
    Docker(DockerContainer),
    Containerd(ContainerdContainer),
    CriO(CriOContainer),
}

/// get a container object according to args.
pub(crate) async fn get_container(
    container_id_opt: Option<&String>,
    container_runtime_opt: Option<&String>,
) -> Result<Option<Container>> {
    if let (Some(container_id), Some(container_runtime)) = (container_id_opt, container_runtime_opt)
    {
        let container_id = container_id.to_string();
        match container_runtime.as_str() {
            "docker" => Ok(Some(Container::Docker(
                DockerContainer::from_id(container_id).await?,
            ))),
            "containerd" => Ok(Some(Container::Containerd(ContainerdContainer {
                container_id,
            }))),
            "cri-o" => Ok(Some(Container::CriO(CriOContainer::from_id(container_id)))),
            _ => Err(AgentError::NotFound(format!(
                "Unknown runtime {container_runtime:?}"
            ))),
        }
    } else {
        Ok(None)
    }
}

#[derive(Debug)]
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

    async fn pause(&self) -> Result<()> {
        self.client
            .pause_container(&self.container_id)
            .await
            .map_err(From::from)
    }

    async fn unpause(&self) -> Result<()> {
        self.client
            .unpause_container(&self.container_id)
            .await
            .map_err(From::from)
    }
}

#[derive(Debug)]
pub(crate) struct ContainerdContainer {
    container_id: String,
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

    async fn pause(&self) -> Result<()> {
        let mut client = self.get_task_client().await?;
        let container_id = self.container_id.to_string();
        let request = PauseTaskRequest { container_id };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
        client.pause(request).await?;
        Ok(())
    }

    async fn unpause(&self) -> Result<()> {
        let mut client = self.get_task_client().await?;
        let container_id = self.container_id.to_string();
        let request = ResumeTaskRequest { container_id };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
        client.resume(request).await?;
        Ok(())
    }
}

#[tracing::instrument(level = "trace")]
pub fn set_namespace(ns_path: PathBuf) -> Result<()> {
    let fd: RawFd = File::open(ns_path)?.into_raw_fd();
    trace!("set_namespace -> fd {:#?}", fd);

    setns(fd, nix::sched::CloneFlags::CLONE_NEWNET)?;
    Ok(())
}

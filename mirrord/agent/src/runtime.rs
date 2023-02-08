use std::{
    fs::File,
    os::unix::io::{IntoRawFd, RawFd},
    path::PathBuf,
};

use async_trait::async_trait;
use bollard::{container::InspectContainerOptions, Docker, API_DEFAULT_VERSION};
use containerd_client::{
    connect,
    services::v1::{tasks_client::TasksClient, GetRequest, PauseTaskRequest, ResumeTaskRequest},
    tonic::{transport::Channel, Request},
    with_namespace,
};
use enum_dispatch::enum_dispatch;
use nix::sched::setns;
use tracing::trace;

use crate::error::{AgentError, Result};

const CONTAINERD_SOCK_PATH: &str = "/host/run/containerd/containerd.sock";
const CONTAINERD_ALTERNATIVE_SOCK_PATH: &str = "/host/run/dockershim.sock";
const CONTAINERD_K3S_SOCK_PATH: &str = "/host/run/k3s/containerd/containerd.sock";

const DEFAULT_CONTAINERD_NAMESPACE: &str = "k8s.io";

#[async_trait]
#[enum_dispatch]
pub(crate) trait ContainerRuntime {
    /// Get the external pid of the container.
    async fn get_pid(&self) -> Result<u64>;
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

#[async_trait]
impl ContainerRuntime for DockerContainer {
    async fn get_pid(&self) -> Result<u64> {
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
        Ok(pid)
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

impl ContainerdContainer {
    async fn get_client() -> Result<TasksClient<Channel>> {
        let channel = match connect(CONTAINERD_SOCK_PATH).await {
            Ok(channel) => channel,
            Err(_) => match connect(CONTAINERD_ALTERNATIVE_SOCK_PATH).await {
                Ok(channel) => channel,
                Err(_) => connect(CONTAINERD_K3S_SOCK_PATH).await?,
            },
        };
        Ok(TasksClient::new(channel))
    }
}

#[async_trait]
impl ContainerRuntime for ContainerdContainer {
    async fn get_pid(&self) -> Result<u64> {
        let mut client = Self::get_client().await?;
        let container_id = self.container_id.to_string();
        let request = GetRequest {
            container_id,
            ..Default::default()
        };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
        let response = client.get(request).await?;
        let pid = response
            .into_inner()
            .process
            .ok_or_else(|| AgentError::NotFound("No pid found!".to_string()))?
            .pid;

        Ok(pid as u64)
    }

    async fn pause(&self) -> Result<()> {
        let mut client = Self::get_client().await?;
        let container_id = self.container_id.to_string();
        let request = PauseTaskRequest { container_id };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
        client.pause(request).await?;
        Ok(())
    }

    async fn unpause(&self) -> Result<()> {
        let mut client = Self::get_client().await?;
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

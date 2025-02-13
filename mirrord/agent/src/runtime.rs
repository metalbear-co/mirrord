use std::{collections::HashMap, ops::Not};

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
use nix::NixPath;
use oci_spec::runtime::Spec;
use tokio::{fs::read_dir, net::UnixStream};
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;
use tracing::Level;

use crate::{env::parse_raw_env, runtime::crio::CriOContainer};

mod crio;
mod error;

pub(crate) use error::{ContainerRuntimeError, ContainerRuntimeResult};

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

    /// Id of the container.
    pub(crate) id: String,
}

impl ContainerInfo {
    pub(crate) fn new(pid: u64, env: HashMap<String, String>, id: String) -> Self {
        ContainerInfo { pid, env, id }
    }
}

#[enum_dispatch]
pub(crate) trait ContainerRuntime {
    /// Get information about the container (pid, env).
    async fn get_info(&self) -> ContainerRuntimeResult<ContainerInfo>;
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
    container_runtime: &str,
) -> ContainerRuntimeResult<Container> {
    match container_runtime {
        "docker" => Ok(Container::Docker(
            DockerContainer::from_id(container_id).await?,
        )),
        "containerd" => Ok(Container::Containerd(ContainerdContainer { container_id })),
        "cri-o" => Ok(Container::CriO(CriOContainer::from_id(container_id))),
        other => Err(ContainerRuntimeError::unknown_runtime(other)),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DockerContainer {
    container_id: String,
    client: Docker,
}

impl DockerContainer {
    async fn from_id(container_id: String) -> ContainerRuntimeResult<Self> {
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
            )
            .map_err(ContainerRuntimeError::docker)?,
        };

        Ok(DockerContainer {
            container_id,
            client,
        })
    }
}

impl ContainerRuntime for DockerContainer {
    async fn get_info(&self) -> ContainerRuntimeResult<ContainerInfo> {
        let inspect_options = Some(InspectContainerOptions { size: false });
        let inspect_response = self
            .client
            .inspect_container(&self.container_id, inspect_options)
            .await
            .map_err(ContainerRuntimeError::docker)?;

        let pid = inspect_response
            .state
            .and_then(|state| state.pid)
            .and_then(|pid| if pid > 0 { Some(pid as u64) } else { None })
            .ok_or_else(|| {
                ContainerRuntimeError::docker("pid not found in the runtime response")
            })?;

        let raw_env = inspect_response
            .config
            .and_then(|config| config.env)
            .ok_or_else(|| {
                ContainerRuntimeError::docker("env not found in the runtime response")
            })?;
        let env_vars = parse_raw_env(&raw_env);

        Ok(ContainerInfo::new(pid, env_vars, self.container_id.clone()))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContainerdContainer {
    container_id: String,
}

async fn connect(path: impl AsRef<std::path::Path>) -> ContainerRuntimeResult<Channel> {
    let path = path.as_ref().to_path_buf();

    Endpoint::try_from("http://localhost")
        .map_err(ContainerRuntimeError::containerd)?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async {
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(
                    UnixStream::connect(path).await?,
                ))
            }
        }))
        .await
        .map_err(ContainerRuntimeError::containerd)
}

/// Connects to the given containerd socket
/// and returns the client only if the given container
/// exists.
async fn connect_and_find_container(
    container_id: String,
    sock_path: impl AsRef<std::path::Path>,
) -> ContainerRuntimeResult<Channel> {
    let channel = connect(sock_path).await?;
    let mut client = TasksClient::new(channel.clone());
    let request = GetRequest {
        container_id,
        ..Default::default()
    };
    let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
    client
        .get(request)
        .await
        .map_err(ContainerRuntimeError::containerd)?;
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
    async fn get_channel(&self) -> ContainerRuntimeResult<Channel> {
        for sock_path in CONTAINERD_SOCK_PATHS {
            if let Ok(channel) =
                connect_and_find_container(self.container_id.clone(), sock_path).await
            {
                return Ok(channel);
            }
        }
        Err(ContainerRuntimeError::containerd(
            r#"Couldn't find containerd socket to use, please open a bug report
            providing information on how you installed k8s and if you know where
            the containerd socket is"#,
        ))
    }

    async fn get_task_client(&self) -> ContainerRuntimeResult<TasksClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(TasksClient::new(channel))
    }

    async fn get_container_client(&self) -> ContainerRuntimeResult<ContainersClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ContainersClient::new(channel))
    }
}

impl ContainerRuntime for ContainerdContainer {
    async fn get_info(&self) -> ContainerRuntimeResult<ContainerInfo> {
        let mut client = self.get_task_client().await?;
        let container_id = self.container_id.to_string();
        let request = GetRequest {
            container_id,
            ..Default::default()
        };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
        let pid = client
            .get(request)
            .await
            .map_err(ContainerRuntimeError::containerd)?
            .into_inner()
            .process
            .ok_or_else(|| {
                ContainerRuntimeError::containerd(
                    "process description not found in runtime response",
                )
            })?
            .pid;

        let mut client = self.get_container_client().await?;
        let request = GetContainerRequest {
            id: self.container_id.to_string(),
        };
        let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);

        let spec: Spec = client
            .get(request)
            .await
            .map_err(ContainerRuntimeError::containerd)?
            .into_inner()
            .container
            .and_then(|c| c.spec)
            .ok_or_else(|| {
                ContainerRuntimeError::containerd("container spec not found in runtime response")
            })
            .and_then(|s| {
                serde_json::from_slice(&s.value).map_err(ContainerRuntimeError::containerd)
            })?;

        let env_vars = extract_env_from_containerd_spec(&spec).ok_or_else(|| {
            ContainerRuntimeError::containerd("env not found in container runtime response")
        })?;

        Ok(ContainerInfo::new(
            pid as u64,
            env_vars,
            self.container_id.clone(),
        ))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EphemeralContainer {
    pub(crate) container_id: String,
}

/// Searches for the `pid` of the target container when the agent is running in `Mode::Ephemeral`.
///
/// It does so by navigating through the dirs in `/proc`, skipping over non-number dirs (like
/// `/proc/fs`), or when the next dir entry returns an error (since this probably means that
/// it's not a dir we should be interested in).
///
/// To find the `pid`, we look into `/proc/{pid}/cgroup`, searching for a string that matches
/// `container_id`.
///
/// Defaults to returning `1` when we cannot find the `pid`.
#[tracing::instrument(level = Level::DEBUG, ret, err)]
async fn find_pid_for_ephemeral(container_id: &str) -> Result<u64, tokio::io::Error> {
    if container_id.is_empty().not() {
        // The only error we don't ignore here, since if this fails, we can't do the `pid`
        // search, and this'll break mirrord remote file operations.
        let mut dir_entries = read_dir("/proc").await?;

        loop {
            match dir_entries.next_entry().await {
                Ok(Some(dir)) => {
                    match dir
                        .path()
                        .to_string_lossy()
                        .split_terminator("/")
                        .last()
                        .map(|path| path.parse::<u64>())
                    {
                        Some(Ok(potential_pid)) => {
                            let mut cgroup_path = dir.path();
                            cgroup_path.push("cgroup");

                            if tokio::fs::read_to_string(cgroup_path)
                                .await
                                .map(|read| read.contains(container_id))
                                .is_ok_and(|found_id| found_id)
                            {
                                return Ok(potential_pid);
                            } else {
                                tracing::debug!(?potential_pid, "the pid found");
                                continue;
                            }
                        }
                        // Skip over `parse` errors, since this means that this dir is not a
                        // `/proc/{pid}`.
                        _ => {
                            tracing::debug!("skipping not-number!");
                            continue;
                        }
                    }
                }
                Ok(None) => return Ok(1),
                Err(fail) => {
                    tracing::warn!(?fail, "Searching for container pid!");
                    continue;
                }
            }
        }
    } else {
        Ok(1)
    }
}

impl ContainerRuntime for EphemeralContainer {
    /// When running on ephemeral, root pid is either set to `1`, or we must search for it using the
    /// `container_id`, when `shareProcessNamespace` is being used. Env is the current process' env.
    /// (we copy it from the k8s spec)
    #[tracing::instrument(level = Level::DEBUG, ret, err)]
    async fn get_info(&self) -> ContainerRuntimeResult<ContainerInfo> {
        Ok(ContainerInfo::new(
            find_pid_for_ephemeral(&self.container_id)
                .await
                .unwrap_or(1),
            std::env::vars().collect(),
            self.container_id.clone(),
        ))
    }
}

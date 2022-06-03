use std::{
    fs::File,
    os::unix::io::{IntoRawFd, RawFd},
    path::PathBuf,
};

use anyhow::Result;
use bollard::{container::InspectContainerOptions, Docker};
use containerd_client::{
    connect,
    services::v1::{tasks_client::TasksClient, GetRequest},
    tonic::Request,
    with_namespace,
};
use nix::sched::setns;
use tracing::debug;

use crate::error::AgentError;

pub(crate) enum Runtime {}

impl Runtime {
    pub async fn get_container_pid(
        container_id: &str,
        container_runtime: &str,
    ) -> Result<PathBuf, AgentError> {
        match container_runtime {
            "docker" => get_docker_container_pid(container_id.to_string()).await,
            "containerd" => get_containerd_container_pid(container_id.to_string()).await,
            _ => Err(AgentError::NotFound(format!(
                "Unknown runtime {}",
                container_runtime
            ))),
        }
    }
}

pub fn set_namespace(ns_path: PathBuf) -> Result<()> {
    debug!("set_namespace -> namespace path {:#?}", ns_path);
    let fd: RawFd = File::open(ns_path)?.into_raw_fd();
    debug!("set_namespace -> fd {:#?}", fd);

    setns(fd, nix::sched::CloneFlags::CLONE_NEWNET)?;
    Ok(())
}

async fn get_docker_container_pid(container_id: String) -> Result<PathBuf, AgentError> {
    let client = Docker::connect_with_local_defaults()?;
    let inspect_options = Some(InspectContainerOptions { size: false });
    let inspect_response = client
        .inspect_container(&container_id, inspect_options)
        .await?;

    let pid = inspect_response
        .state
        .and_then(|state| state.pid)
        .ok_or_else(|| AgentError::NotFound(format!("No pid found!")))?;

    Ok(PathBuf::from(pid.to_string()))
}

async fn get_containerd_container_pid(container_id: String) -> Result<PathBuf, AgentError> {
    let (containerd_socket, default_namespace) = ("/run/containerd/containerd.sock", "k8s.io");
    let channel = connect(&containerd_socket).await?;
    let mut client = TasksClient::new(channel);
    let request = GetRequest {
        container_id,
        ..Default::default()
    };
    let request = with_namespace!(request, default_namespace);
    let response = client.get(request).await?;
    let process = response
        .into_inner()
        .process
        .ok_or_else(|| AgentError::NotFound(format!("PID not found!")))?;

    Ok(PathBuf::from(process.pid.to_string()))
}

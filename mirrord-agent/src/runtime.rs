use std::{
    fs::File,
    os::unix::io::{IntoRawFd, RawFd},
    path::PathBuf,
};

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

const CONTAINERD_SOCK_PATH: &str = "/run/containerd/containerd.sock";
const DEFAULT_CONTAINERD_NAMESPACE: &str = "k8s.io";

pub async fn get_container_pid(
    container_id: &str,
    container_runtime: &str,
) -> Result<u64, AgentError> {
    match container_runtime {
        "docker" => get_docker_container_pid(container_id.to_string()).await,
        "containerd" => get_containerd_container_pid(container_id.to_string()).await,
        _ => Err(AgentError::NotFound(format!(
            "Unknown runtime {}",
            container_runtime
        ))),
    }
}

async fn get_docker_container_pid(container_id: String) -> Result<u64, AgentError> {
    let client = Docker::connect_with_local_defaults()?;
    let inspect_options = Some(InspectContainerOptions { size: false });
    let inspect_response = client
        .inspect_container(&container_id, inspect_options)
        .await?;

    let pid = inspect_response
        .state
        .and_then(|state| state.pid)
        .and_then(|pid| if pid > 0 { Some(pid as u64) } else { None })
        .ok_or_else(|| AgentError::NotFound(format!("No pid found!")))?;
    Ok(pid)
}

async fn get_containerd_container_pid(container_id: String) -> Result<u64, AgentError> {
    let channel = connect(CONTAINERD_SOCK_PATH).await?;
    let mut client = TasksClient::new(channel);
    let request = GetRequest {
        container_id,
        ..Default::default()
    };
    let request = with_namespace!(request, DEFAULT_CONTAINERD_NAMESPACE);
    let response = client.get(request).await?;
    let pid = response
        .into_inner()
        .process
        .ok_or_else(|| AgentError::NotFound(format!("No pid found!")))?
        .pid;

    Ok(pid as u64)
}

pub fn set_namespace(ns_path: PathBuf) -> Result<(), AgentError> {
    let fd: RawFd = File::open(ns_path)?.into_raw_fd();
    debug!("set_namespace -> fd {:#?}", fd);

    setns(fd, nix::sched::CloneFlags::CLONE_NEWNET)?;
    Ok(())
}

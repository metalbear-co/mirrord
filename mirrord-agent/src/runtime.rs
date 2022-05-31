use std::{
    fs::File,
    os::unix::io::{IntoRawFd, RawFd},
};

use anyhow::{anyhow, Result};
use bollard::{container::InspectContainerOptions, Docker};
use containerd_client::{
    connect,
    services::v1::{tasks_client::TasksClient, GetRequest},
    tonic::Request,
    with_namespace,
};
use nix::sched::setns;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Namespace {
    #[serde(rename = "type")]
    ns_type: String,
    path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct LinuxInfo {
    namespaces: Vec<Namespace>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Spec {
    linux: LinuxInfo,
}

pub(crate) enum Runtime {}

impl Runtime {
    pub async fn get_container_pid(container_id: &str, container_runtime: &str) -> Result<u32> {
        match container_runtime {
            "docker" => get_docker_container_pid(container_id.to_string()).await,
            "containerd" => get_containerd_container_pid(container_id.to_string()).await,
            _ => Err(anyhow!("Unknown runtime: {}", container_runtime)),
        }
    }
}

pub fn set_namespace(ns_path: String) -> Result<()> {
    let fd: RawFd = File::open(ns_path)?.into_raw_fd();
    setns(fd, nix::sched::CloneFlags::CLONE_NEWNET)?;
    Ok(())
}

pub fn get_namespace(pid: u32, ns_type: &str) -> String {
    format!("/proc/{}/ns/{}", pid, ns_type)
}

async fn get_docker_container_pid(container_id: String) -> Result<u32> {
    let client = Docker::connect_with_local_defaults()?;
    let inspect_options = Some(InspectContainerOptions { size: false });
    let inspect_response = client
        .inspect_container(&container_id, inspect_options)
        .await?;

    let pid = inspect_response
        .state
        .and_then(|state| state.pid)
        .ok_or_else(|| anyhow!("No pid found"))
        .unwrap();
    Ok(pid as u32)
}

async fn get_containerd_container_pid(container_id: String) -> Result<u32> {
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
        .ok_or_else(|| anyhow!("No pid found"))?;

    Ok(process.pid)
}

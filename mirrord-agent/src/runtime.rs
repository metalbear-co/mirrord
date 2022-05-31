use std::{
    fs::File,
    os::unix::io::{IntoRawFd, RawFd},
};

use anyhow::{anyhow, Result};
use bollard::{container::InspectContainerOptions, Docker};
use containerd_client::{
    connect,
    services::v1::{containers_client::ContainersClient, GetContainerRequest},
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

pub(crate) enum Runtime {
    Containerd(String),
    Docker(String),
}

impl Runtime {
    pub async fn get_namespace(&self) -> Result<String> {
        match self {
            Runtime::Containerd(container_id) => {
                self.get_containerd_namespace(container_id.to_string())
                    .await
            }
            Runtime::Docker(container_id) => {
                self.get_docker_namespace(container_id.to_string()).await
            }
        }
    }

    pub fn set_namespace(&self, ns_path: String) -> Result<()> {
        let fd: RawFd = File::open(ns_path)?.into_raw_fd();
        setns(fd, nix::sched::CloneFlags::CLONE_NEWNET)?;
        Ok(())
    }

    async fn get_containerd_namespace(&self, container_id: String) -> Result<String> {
        let (containerd_socket, default_namespace) = ("/run/containerd/containerd.sock", "k8s.io");
        let channel = connect(&containerd_socket).await?;
        let mut client = ContainersClient::new(channel);
        let request = GetContainerRequest {
            id: container_id.clone(),
        };
        let request = with_namespace!(request, default_namespace);
        let resp = client.get(request).await?;
        let resp = resp.into_inner();
        let container = resp
            .container
            .ok_or_else(|| anyhow!("container not found"))?;
        let spec: Spec = serde_json::from_slice(
            &container
                .spec
                .ok_or_else(|| anyhow!("invalid data from containerd"))?
                .value,
        )?;
        let ns_path = spec
            .linux
            .namespaces
            .iter()
            .find(|ns| ns.ns_type == "network")
            .ok_or_else(|| anyhow!("network namespace not found"))?
            .path
            .as_ref()
            .ok_or_else(|| anyhow!("no network namespace path"))?;
        Ok(ns_path.to_owned())
    }

    async fn get_docker_namespace(&self, container_id: String) -> Result<String> {
        let client = Docker::connect_with_local_defaults()?;
        let inspect_options = Some(InspectContainerOptions { size: false });
        let inspect_response = client
            .inspect_container(&container_id, inspect_options)
            .await?;
        let pid = inspect_response.state.unwrap().pid.unwrap();

        let ns_path = format!("/proc/{}/ns/net", pid);
        Ok(ns_path)
    }
}

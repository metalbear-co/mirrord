use anyhow::Result;
use async_trait::async_trait;
use bollard::{container::InspectContainerOptions, Docker};

use crate::namespace::Namespace;

pub struct DockerRuntime {
    container_id: String,
}

impl DockerRuntime {
    pub fn new(container_id: String) -> Self {
        DockerRuntime { container_id }
    }
}

#[async_trait]
impl Namespace for DockerRuntime {
    async fn get_namespace(&self) -> Result<String> {
        let client = Docker::connect_with_local_defaults()?;
        let inspect_options = Some(InspectContainerOptions { size: false });
        let inspect_response = client
            .inspect_container(&self.container_id, inspect_options)
            .await?;
        let pid = inspect_response.state.unwrap().pid.unwrap();

        let ns_path = format!("/proc/{}/ns/net", pid);
        Ok(ns_path)
    }
}

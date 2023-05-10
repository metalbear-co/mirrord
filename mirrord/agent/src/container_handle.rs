use std::collections::HashMap;

use crate::{
    error::Result,
    runtime::{Container, ContainerInfo, ContainerRuntime},
};

#[derive(Debug)]
pub(crate) struct ContainerHandle {
    container: Container,
    should_pause: bool,
    pid: u64,
    raw_env: HashMap<String, String>,
}

impl ContainerHandle {
    pub(crate) async fn new(container: Container, should_pause: bool) -> Result<Self> {
        let ContainerInfo { pid, env: raw_env } = container.get_info().await?;

        Ok(Self {
            container,
            should_pause,
            pid,
            raw_env,
        })
    }

    pub(crate) fn pid(&self) -> u64 {
        self.pid
    }

    pub(crate) fn raw_env(&self) -> &HashMap<String, String> {
        &self.raw_env
    }

    pub(crate) fn should_pause(&self) -> bool {
        self.should_pause
    }

    pub(crate) async fn pause(&self) -> Result<()> {
        self.container.pause().await
    }

    pub(crate) async fn unpause(&self) -> Result<()> {
        self.container.unpause().await
    }
}

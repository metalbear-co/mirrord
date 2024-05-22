use std::{collections::HashMap, sync::Arc};

use crate::{
    error::Result,
    runtime::{Container, ContainerInfo, ContainerRuntime},
};

#[derive(Debug)]
struct Inner {
    /// The managed container.
    container: Container,
    /// Cached process ID of the container.
    pid: u64,
    /// Cached environment of the container.
    raw_env: HashMap<String, String>,
    /// Watch for using in the drop
    watch: drain::Watch,
}

/// Handle to the container targeted by the agent.
/// Exposes some cached info about the container and allows pausing it according to clients'
/// requests.
#[derive(Debug, Clone)]
pub(crate) struct ContainerHandle(Arc<Inner>);

impl ContainerHandle {
    /// Retrieve info about the container and initialize this struct.
    #[tracing::instrument(level = "trace")]
    pub(crate) async fn new(container: Container, watch: drain::Watch) -> Result<Self> {
        let ContainerInfo { pid, env: raw_env } = container.get_info().await?;

        let inner = Inner {
            container,
            pid,
            raw_env,
            watch,
        };

        Ok(Self(inner.into()))
    }

    /// Return the process ID of the container.
    pub(crate) fn pid(&self) -> u64 {
        self.0.pid
    }

    /// Return environment variables from the container.
    pub(crate) fn raw_env(&self) -> &HashMap<String, String> {
        &self.0.raw_env
    }
}

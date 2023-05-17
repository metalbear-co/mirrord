use std::{collections::HashMap, sync::Arc};

use futures::executor;
use tokio::sync::RwLock;
use tracing::{error, info};

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
    /// Whether the container is paused.
    paused: RwLock<bool>,
}

/// This is to make sure we don't leave the target container paused when the agent exits.
impl Drop for Inner {
    fn drop(&mut self) {
        let result = executor::block_on(async {
            if *self.paused.read().await {
                info!("Agent exiting with target container paused. Unpausing target container.");
                self.container.unpause().await
            } else {
                Ok(())
            }
        });

        if let Err(err) = result {
            error!("Could not unpause target container while exiting, got error: {err:?}");
        }
    }
}

/// Handle to the container targeted by the agent.
/// Exposes some cached info about the container and allows pausing it according to clients'
/// requests.
#[derive(Debug, Clone)]
pub(crate) struct ContainerHandle(Arc<Inner>);

impl ContainerHandle {
    /// Retrieve info about the container and initialize this struct.
    #[tracing::instrument(level = "trace")]
    pub(crate) async fn new(container: Container) -> Result<Self> {
        let ContainerInfo { pid, env: raw_env } = container.get_info().await?;

        let inner = Inner {
            container,
            pid,
            raw_env,
            paused: Default::default(),
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

    /// Pause or unpause the container.
    /// If the container changed its state, return true.
    /// Otherwise, return false.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) async fn set_paused(&self, paused: bool) -> Result<bool> {
        let mut guard = self.0.paused.write().await;

        match (*guard, paused) {
            (false, true) => self.0.container.pause().await?,
            (true, false) => self.0.container.unpause().await?,
            _ => return Ok(false),
        }

        *guard = paused;

        Ok(true)
    }
}

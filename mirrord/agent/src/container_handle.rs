use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use futures::executor;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::{
    error::Result,
    runtime::{Container, ContainerInfo, ContainerRuntime},
    util::ClientId,
};

#[derive(Debug)]
struct Inner {
    /// The managed container.
    container: Container,
    /// Cached process ID of the container.
    pid: u64,
    /// Cached environment of the container.
    raw_env: HashMap<String, String>,
    /// Set of active pause requests from the clients.
    /// Clients can dynamically request to pause the container and the request remains active until
    /// the client disconnects. When there is at least one active request, the container should
    /// be paused.
    pause_requests: RwLock<HashSet<ClientId>>,
}

/// This is to make sure we don't leave the target container paused if the agent hits an error and
/// exits early without removing all of its clients.
impl Drop for Inner {
    fn drop(&mut self) {
        let result = executor::block_on(async {
            if self.pause_requests.read().await.is_empty() {
                Ok(())
            } else {
                info!("Agent exiting without having removed all of the clients. Unpausing target container.");
                self.container.unpause().await
            }
        });

        if let Err(err) = result {
            error!("Could not unpause target container while exiting early, got error: {err:?}");
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
    pub(crate) async fn new(container: Container) -> Result<Self> {
        let ContainerInfo { pid, env: raw_env } = container.get_info().await?;

        let inner = Inner {
            container,
            pid,
            raw_env,
            pause_requests: Default::default(),
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

    /// Add the pause request to the set of active requests.
    /// If the set became non-empty, pause the container and return true. Otherwise, return false.
    pub(crate) async fn request_pause(&self, client_id: ClientId) -> Result<bool> {
        let mut guard = self.0.pause_requests.write().await;
        let do_pause = guard.is_empty();

        if do_pause {
            self.0.container.pause().await?;
        }

        guard.insert(client_id);

        Ok(do_pause)
    }

    /// Remove pause request of the given client from the set of active requests.
    /// If the set became empty, unpause the container and return true. Otherwise, return false.
    pub(crate) async fn client_disconnected(&self, client_id: ClientId) -> Result<bool> {
        let mut guard = self.0.pause_requests.write().await;
        let do_unpause = guard.contains(&client_id) && guard.len() == 1;

        if do_unpause {
            self.0.container.unpause().await?;
        }

        guard.remove(&client_id);

        Ok(do_unpause)
    }
}

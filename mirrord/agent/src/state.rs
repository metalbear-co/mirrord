use std::collections::{HashMap, HashSet};

use futures::executor;
use tokio::{net::TcpStream, sync::mpsc::Sender, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, trace};

use crate::{
    cli::Args,
    client::ClientConnectionHandler,
    dns::DnsRequest,
    error::{AgentError, Result},
    runtime::{get_container, Container, ContainerInfo, ContainerRuntime},
    sniffer::SnifferCommand,
    steal::StealerCommand,
    util::{ClientId, IndexAllocator},
};

/// Keeps track of connected clients.
/// If pausing target, also pauses and unpauses when number of clients changes from or to 0.
#[derive(Debug)]
pub struct State {
    clients: HashSet<ClientId>,
    index_allocator: IndexAllocator<ClientId>,
    /// Was the pause argument passed? If true, will pause the container when no clients are
    /// connected.
    should_pause: bool,
    /// This is an option because it is acceptable not to pass a container runtime and id if not
    /// pausing. When those args are not passed, container is None.
    container: Option<Container>,
}

/// This is to make sure we don't leave the target container paused if the agent hits an error and
/// exits early without removing all of its clients.
impl Drop for State {
    fn drop(&mut self) {
        if self.should_pause && !self.no_clients_left() {
            info!(
                "Agent exiting without having removed all the clients. Unpausing target container."
            );
            if let Err(err) = executor::block_on(self.container.as_ref().unwrap().unpause()) {
                error!(
                    "Could not unpause target container while exiting early, got error: {err:?}"
                );
            }
        }
    }
}

impl State {
    /// Returns Err if container runtime operations failed or if the `pause` arg was passed, but
    /// the container info (runtime and id) was not.
    pub async fn new(args: &Args) -> Result<State> {
        let container =
            get_container(args.container_id.as_ref(), args.container_runtime.as_ref()).await?;
        if container.is_none() && args.pause {
            return Err(AgentError::MissingContainerInfo);
        }
        Ok(State {
            clients: HashSet::new(),
            index_allocator: IndexAllocator::new(),
            should_pause: args.pause,
            container,
        })
    }

    /// Get the external pid + env of the target container, if container info available.
    pub async fn get_container_info(&self) -> Result<Option<ContainerInfo>> {
        if self.container.is_some() {
            let container = self.container.as_ref().unwrap();
            let info = container.get_info().await?;
            Ok(Some(info))
        } else {
            Ok(None)
        }
    }

    /// If there are clientIDs left, insert new one and return it.
    /// If there were no clients before, and there is a Pauser, start pausing.
    /// Propagate container runtime errors.
    pub async fn new_client(&mut self) -> Result<ClientId> {
        match self.generate_id() {
            None => Err(AgentError::ConnectionLimitReached),
            Some(new_id) => {
                self.clients.insert(new_id.to_owned());
                if self.clients.len() == 1 {
                    // First client after no clients.
                    if self.should_pause {
                        self.container.as_ref().unwrap().pause().await?;
                        trace!("First client connected - pausing container.")
                    }
                }
                Ok(new_id)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new_connection(
        &mut self,
        stream: TcpStream,
        sniffer_command_tx: Sender<SnifferCommand>,
        stealer_command_tx: Sender<StealerCommand>,
        cancellation_token: CancellationToken,
        dns_sender: Sender<DnsRequest>,
        ephemeral_container: bool,
        pid: Option<u64>,
        env: HashMap<String, String>,
    ) -> Result<Option<JoinHandle<u32>>> {
        match self.new_client().await {
            Ok(client_id) => {
                let client = tokio::spawn(async move {
                    match ClientConnectionHandler::new(
                        client_id,
                        pid,
                        ephemeral_container,
                        sniffer_command_tx,
                        stealer_command_tx,
                        dns_sender,
                        env,
                        cancellation_token,
                    )
                    .serve(stream)
                    .await
                    {
                        Ok(_) => {
                            trace!(
                                "ClientConnectionHandler::start -> Client {client_id}
    disconnected"
                            );
                        }
                        Err(e) => {
                            error!(
                                "ClientConnectionHandler::start -> Client {client_id}
    disconnected with error: {e}"
                            );
                        }
                    }
                    client_id
                });
                Ok(Some(client))
            }
            Err(AgentError::ConnectionLimitReached) => {
                error!("start_client -> Ran out of connections, dropping new connection");
                Ok(None)
            }
            // Propagate all errors that are not ConnectionLimitReached.
            Err(err) => Err(err),
        }
    }

    fn generate_id(&mut self) -> Option<ClientId> {
        self.index_allocator.next_index()
    }

    /// If that was the last client and we are pausing, stop pausing.
    /// Propagate container runtime errors.
    pub async fn remove_client(&mut self, client_id: ClientId) -> Result<()> {
        self.clients.remove(&client_id);
        self.index_allocator.free_index(client_id);
        if self.no_clients_left() {
            // resume container (stop stopping).
            if self.should_pause {
                self.container.as_ref().unwrap().unpause().await?;
                trace!("Last client disconnected - resuming container.")
            }
        }
        Ok(())
    }

    pub fn no_clients_left(&self) -> bool {
        self.clients.is_empty()
    }
}

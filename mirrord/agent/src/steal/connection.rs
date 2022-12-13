use std::{
    collections::HashSet,
    io,
    net::{Ipv4Addr, SocketAddr},
};

use bytes::Bytes;
use iptables::IPTables;
use mirrord_protocol::{
    tcp::{NewTcpConnection, TcpClose},
    ResponseError::PortAlreadyStolen,
};
use streammap_ext::StreamMap;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::error;

use super::*;
use crate::AgentError::AgentInvariantViolated;

/// Created once per agent during initialization.
///
/// Runs as a separate thread while the agent lives.
///
/// - (agent -> stealer) communication is handled by [`command_rx`];
/// - (stealer -> agent) communication is handled by [`client_senders`], and the [`Sender`] channels
///   come inside [`StealerCommand`]s through  [`command_rx`];
pub(crate) struct TcpConnectionStealer {
    /// Maps a port to its active subscription.
    port_subscriptions: HashMap<Port, StealSubscription>,

    /// Communication between (agent -> stealer) task.
    ///
    /// The agent controls the stealer task through [`TcpStealerAPI::command_tx`].
    command_rx: Receiver<StealerCommand>,

    /// Connected clients (layer instances) and the channels which the stealer task uses to send
    /// back messages (stealer -> agent -> layer).
    clients: HashMap<ClientId, Sender<DaemonTcp>>,
    index_allocator: IndexAllocator<ConnectionId>,

    /// Intercepts the connections, instead of letting them go through their normal pathways, this
    /// is used to steal the traffic.
    stealer: TcpListener,

    /// Set of rules the agent uses to steal traffic from through the
    /// [`TcpConnectionStealer::stealer`] listener.
    /// None when there are no subscribers.
    iptables: Option<SafeIpTables<IPTables>>,

    /// Used to send data back to the original remote connection.
    write_streams: HashMap<ConnectionId, WriteHalf<TcpStream>>,

    /// Used to read data from the remote connections.
    read_streams: StreamMap<ConnectionId, ReaderStream<ReadHalf<TcpStream>>>,

    /// Associates a `ConnectionId` with a `ClientID`, so we can send the data we read from
    /// [`TcpConnectionStealer::read_streams`] to the appropriate client (layer).
    connection_clients: HashMap<ConnectionId, ClientId>,

    /// Map a `ClientId` to a set of its `ConnectionId`s. Used to close all connections when
    /// client closes.
    client_connections: HashMap<ClientId, HashSet<ConnectionId>>,
}

impl TcpConnectionStealer {
    /// Initializes a new [`TcpConnectionStealer`] fields, but doesn't start the actual working
    /// task (call [`TcpConnectionStealer::start`] to do so).
    #[tracing::instrument(level = "trace")]
    pub(crate) async fn new(
        command_rx: Receiver<StealerCommand>,
        pid: Option<u64>,
    ) -> Result<Self, AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        Ok(Self {
            port_subscriptions: HashMap::with_capacity(4),
            command_rx,
            clients: HashMap::with_capacity(8),
            index_allocator: IndexAllocator::new(),
            stealer: TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?,
            iptables: None, // Initialize on first subscription.
            write_streams: HashMap::with_capacity(8),
            read_streams: StreamMap::with_capacity(8),
            connection_clients: HashMap::with_capacity(8),
            client_connections: HashMap::with_capacity(8),
        })
    }

    /// Get a result with a reference to the iptables.
    /// Should only be called while there are subscribers (otherwise self.iptables is None).
    fn iptables(&self) -> Result<&SafeIpTables<IPTables>> {
        debug_assert!(self.iptables.is_some()); // is_some as long as there are subs
        self.iptables.as_ref().ok_or(AgentInvariantViolated)
    }

    /// Runs the tcp traffic stealer loop.
    ///
    /// The loop deals with 3 different paths:
    ///
    /// 1. Receiving [`StealerCommand`]s and calling [`TcpConnectionStealer::handle_command`];
    ///
    /// 2. Accepting remote connections through the [`TcpConnectionStealer::stealer`]
    /// [`TcpListener`]. We steal traffic from the created streams.
    ///
    /// 3. Handling the cancellation of the whole stealer thread.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) async fn start(
        mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), AgentError> {
        loop {
            select! {
                command = self.command_rx.recv() => {
                    if let Some(command) = command {
                        self.handle_command(command).await?;
                    } else { break; }
                },
                // Accepts a connection that we're going to be stealing traffic from.
                accept = self.stealer.accept() => {
                    match accept {
                        Ok(accept) => {
                            self.incoming_connection(accept).await?;
                        }
                        Err(fail) => {
                            error!("Something went wrong while accepting a connection {:#?}", fail);
                            break;
                        }
                    }
                }
                Some((connection_id, incoming_data)) = self.read_streams.next() => {
                    // TODO: Should we spawn a task to forward the data?
                    if let Err(fail) = self.forward_incoming_tcp_data(connection_id, incoming_data).await {
                        error!("Failed reading incoming tcp data with {fail:#?}!");
                    }
                }
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    /// Forwards data from a remote stream to the client with `connection_id`.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn forward_incoming_tcp_data(
        &mut self,
        connection_id: ConnectionId,
        incoming_data: Option<Result<Bytes, io::Error>>,
    ) -> Result<(), AgentError> {
        // Create a message to send to the client, or propagate an error.
        let daemon_tcp_message = incoming_data
            .map(|incoming_data_result| match incoming_data_result {
                Ok(bytes) => Ok(DaemonTcp::Data(TcpData {
                    connection_id,
                    bytes: bytes.to_vec(),
                })),
                Err(fail) => {
                    error!("connection id {connection_id:?} read error: {fail:?}");
                    Err(AgentError::IO(fail))
                }
            })
            .unwrap_or(Ok(DaemonTcp::Close(TcpClose { connection_id })))?;

        if let Some(daemon_tx) = self
            .connection_clients
            .get(&connection_id)
            .and_then(|client_id| self.clients.get(client_id))
        {
            Ok(daemon_tx.send(daemon_tcp_message).await?)
        } else {
            // Either connection_id or client_id does not exist. This would be a bug.
            error!(
                "Internal mirrord error: stealer received data on a connection that was already \
                removed."
            );
            debug_assert!(false);
            Ok(())
        }
    }

    /// Forward the whole connection to given client.
    async fn steal_connection(
        &mut self,
        client_id: ClientId,
        address: SocketAddr,
        port: Port,
        stream: TcpStream,
    ) -> Result<()> {
        let connection_id = self.index_allocator.next_index().unwrap();

        let (read_half, write_half) = tokio::io::split(stream);
        self.write_streams.insert(connection_id, write_half);
        self.read_streams
            .insert(connection_id, ReaderStream::new(read_half));

        self.connection_clients.insert(connection_id, client_id);
        self.client_connections
            .entry(client_id)
            .or_insert_with(HashSet::new)
            .insert(connection_id);

        let new_connection = DaemonTcp::NewConnection(NewTcpConnection {
            connection_id,
            destination_port: port,
            source_port: address.port(),
            address: address.ip(),
        });

        // Send new connection to subscribed layer.
        match self.clients.get(&client_id) {
            Some(daemon_tx) => Ok(daemon_tx.send(new_connection).await?),
            None => {
                // Should not happen.
                debug_assert!(false);
                error!("Internal error: subscriptions of closed client still present.");
                self.close_client(client_id)
            }
        }
    }

    /// Handles a new remote connection that was accepted on the [`TcpConnectionStealer::stealer`]
    /// listener.
    ///
    /// We separate the stream created by accepting the connection into [`ReadHalf`] and
    /// [`WriteHalf`] to handle reading and sending separately.
    ///
    /// Also creates an association between `connection_id` and `client_id` to be used by
    /// [`forward_incoming_tcp_data`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn incoming_connection(
        &mut self,
        (stream, address): (TcpStream, SocketAddr),
    ) -> Result<()> {
        let real_address = orig_dst::orig_dst_addr(&stream)?;

        match self.port_subscriptions.get(&real_address.port()) {
            Some(StealSubscription::Unfiltered(client_id)) => {
                self.steal_connection(client_id.clone(), address, real_address.port(), stream)
                    .await
            }
            Some(StealSubscription::HttpFiltered(manager)) => manager.new_connection(stream),
            // Got connection to port without subscribers. This would be a bug, as we are supposed
            // to set the iptables rules such that we only redirect ports with subscribers.
            None => Err(AgentError::UnexpectedConnection(real_address.port())),
        }
    }

    /// Registers a new layer instance that has the `steal` feature enabled.
    #[tracing::instrument(level = "trace", skip(self, sender))]
    fn new_client(&mut self, client_id: ClientId, sender: Sender<DaemonTcp>) {
        self.clients.insert(client_id, sender);
    }

    /// Initialize iptables member, which creates an iptables chain for our rules.
    fn init_iptables(&mut self) -> Result<()> {
        self.iptables = Some(SafeIpTables::new(iptables::new(false).unwrap())?);
        Ok(())
    }

    /// Add port redirection to iptables to steal `port`.
    fn redirect_port(&mut self, port: Port) -> Result<()> {
        self.iptables()?
            .add_redirect(port, self.stealer.local_addr()?.port())
    }

    fn stop_redirecting_port(&mut self, port: Port) -> Result<()> {
        self.iptables()?
            .remove_redirect(port, self.stealer.local_addr()?.port())
    }

    /// Helper function to handle [`Command::PortSubscribe`] messages.
    ///
    /// Inserts `port` into [`TcpConnectionStealer::iptables`] rules, and subscribes the layer with
    /// `client_id` to steal traffic from it.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn port_subscribe(
        &mut self,
        client_id: ClientId,
        port_steal: PortSteal,
    ) -> Result<(), AgentError> {
        let res = match port_steal {
            PortSteal::Steal(port) => {
                if let Some(sub) = self.port_subscriptions.get(&port) {
                    error!(
                        "Can't steal whole port {port:?} as it is already being stolen: {sub:?}."
                    );
                    Err(PortAlreadyStolen(port))
                } else {
                    if self.port_subscriptions.is_empty() {
                        // Is this the first client?
                        // Initialize IP table only when a client is subscribed.
                        self.init_iptables()?;
                    }
                    self.port_subscriptions
                        .insert(port, StealSubscription::Unfiltered(client_id));
                    self.redirect_port(port)?;
                    Ok(port)
                }
            }
            PortSteal::HttpFilterSteal(port, regex) => {
                match self.port_subscriptions.entry(port).or_insert_with(|| {
                    StealSubscription::HttpFiltered(HttpFilterManager {/* TODO */})
                }) {
                    StealSubscription::Unfiltered(earlier_client) => {
                        error!("Can't filter-steal port {port:?} as it is already being stolen in it's whole by client {earlier_client:?}.");
                        Err(PortAlreadyStolen(port))
                    }
                    StealSubscription::HttpFiltered(manager) => {
                        if manager.is_empty() {
                            // Is this the first client?
                            // Initialize IP table only when a client is subscribed.
                            // TODO: make the initialization internal to SafeIpTables.
                            self.init_iptables()?;
                        }
                        // TODO: manager.insert(client_id, regex)
                        self.redirect_port(port)?;
                        Ok(port)
                    }
                }
            }
        };
        self.send_message_to_single_client(&client_id, DaemonTcp::SubscribeResult(res))
            .await
    }

    /// Helper function to handle [`Command::PortUnsubscribe`] messages.
    ///
    /// Removes `port` from [`TcpConnectionStealer::iptables`] rules, and unsubscribes the layer
    /// with `client_id`.
    #[tracing::instrument(level = "trace", skip(self))]
    fn port_unsubscribe(&mut self, client_id: ClientId, port: Port) -> Result<(), AgentError> {
        let port_unsubscribed = match self.port_subscriptions.get(&port) {
            Some(StealSubscription::HttpFiltered(manager)) => {
                //TODO: manager.remove(client_id)
                manager.is_empty()
            }
            Some(StealSubscription::Unfiltered(subscribed_client))
                if *subscribed_client == client_id =>
            {
                true
            }
            Some(StealSubscription::Unfiltered(_)) | None => {
                warn!("A client tried to unsubscribe from a port it was not subscribed to.");
                false
            }
        };
        if port_unsubscribed {
            // No remaining subscribers on this port.
            self.stop_redirecting_port(port)?;
            self.port_subscriptions.remove(&port);
            if self.port_subscriptions.is_empty() {
                // Was this the last client?
                self.iptables = None; // The Drop impl of iptables cleans up.
            }
        }
        Ok(())
    }

    fn get_client_ports(&self, client_id: ClientId) -> Vec<Port> {
        self.port_subscriptions
            .iter()
            .filter(|(port, sub)| match sub {
                StealSubscription::Unfiltered(port_client) => *port_client == client_id,
                StealSubscription::HttpFiltered(manager) => manager.has_client(client_id),
            })
            .map(|(port, sub)| port.clone())
            .collect()
    }

    /// Removes the client with `client_id` from our list of clients (layers), and also removes
    /// their redirection rules from [`TcpConnectionStealer::iptables`] and all their open
    /// connecitons.
    #[tracing::instrument(level = "trace", skip(self))]
    fn close_client(&mut self, client_id: ClientId) -> Result<(), AgentError> {
        let ports = self.get_client_ports(client_id);
        for port in ports.iter() {
            self.port_unsubscribe(client_id, port.clone())?
        }

        // Close and remove all remaining connections of the closed client.
        if let Some(remaining_connections) = self.client_connections.remove(&client_id) {
            for connection_id in remaining_connections.into_iter() {
                self.remove_connection(connection_id);
            }
        }

        self.clients.remove(&client_id);
        Ok(())
    }

    /// Sends a [`DaemonTcp`] message back to the client with `client_id`.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn send_message_to_single_client(
        &mut self,
        client_id: &ClientId,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        if let Some(sender) = self.clients.get(client_id) {
            sender.send(message).await.map_err(|fail| {
                warn!(
                    "Failed to send message to client {} with {:#?}!",
                    client_id, fail
                );
                let _ = self.close_client(*client_id);
                fail
            })?;
        }

        Ok(())
    }

    /// Write the data received from local app via layer to the stream with end client.
    async fn forward_data(&mut self, tcp_data: TcpData) -> std::result::Result<(), AgentError> {
        if let Some(stream) = self.write_streams.get_mut(&tcp_data.connection_id) {
            stream.write_all(&tcp_data.bytes[..]).await?;
            Ok(())
        } else {
            warn!(
                "Trying to send data to closed connection {:?}",
                tcp_data.connection_id
            );
            Ok(())
        }
    }

    /// Removes the ([`ReadHalf`], [`WriteHalf`]) pair of streams, disconnecting the remote
    /// connection.
    /// Also remove connection from connection mappings and free the connection index.
    /// This method does not remove from client_connections so that it can be called while
    #[tracing::instrument(level = "trace", skip(self))]
    fn remove_connection(&mut self, connection_id: ConnectionId) -> Option<ClientId> {
        self.write_streams.remove(&connection_id);
        self.read_streams.remove(&connection_id);
        self.index_allocator.free_index(connection_id);
        self.connection_clients.remove(&connection_id)
    }

    /// Close the connection, remove the id from all maps and free the id.
    #[tracing::instrument(level = "trace", skip(self))]
    fn connection_unsubscribe(&mut self, connection_id: ConnectionId) {
        if let Some(client_id) = self.remove_connection(connection_id) {
            // Remove the connection from the set of the connections that belong to its client.
            let mut no_connections_left = false;
            self.client_connections
                .entry(client_id)
                .and_modify(|connections| {
                    connections.remove(&connection_id);
                    no_connections_left = connections.is_empty();
                });
            // If we removed the last connection of this client, remove client from map.
            if no_connections_left {
                self.client_connections.remove(&client_id);
            }
        }
    }

    /// Handles [`Command`]s that were received by [`TcpConnectionStealer::command_rx`].
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_command(&mut self, command: StealerCommand) -> Result<(), AgentError> {
        let StealerCommand { client_id, command } = command;

        match command {
            Command::NewClient(daemon_tx) => self.new_client(client_id, daemon_tx),
            Command::ConnectionUnsubscribe(connection_id) => {
                self.connection_unsubscribe(connection_id)
            }
            Command::PortSubscribe(port_steal) => {
                self.port_subscribe(client_id, port_steal).await?
            }
            Command::PortUnsubscribe(port) => self.port_unsubscribe(client_id, port)?,
            Command::ClientClose => self.close_client(client_id)?,
            Command::ResponseData(tcp_data) => self.forward_data(tcp_data).await?,
        }

        Ok(())
    }
}

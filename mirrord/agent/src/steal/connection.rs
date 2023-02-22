use std::{
    collections::HashSet,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use bytes::Bytes;
use fancy_regex::Regex;
use http_body_util::Full;
use hyper::Response;
use iptables::IPTables;
use mirrord_protocol::{
    tcp::{NewTcpConnection, TcpClose},
    RemoteError::BadHttpFilterRegex,
    ResponseError::PortAlreadyStolen,
};
use streammap_ext::StreamMap;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::error;

use super::*;
use crate::{
    error::Result,
    steal::{
        connection::StealSubscription::{HttpFiltered, Unfiltered},
        http::HttpFilterManager,
    },
    AgentError::{AgentInvariantViolated, HttpRequestReceiverClosed},
};

/// The subscriptions to steal traffic from a specific port.
#[derive(Debug)]
enum StealSubscription {
    /// All of the port's traffic goes to this single client.
    Unfiltered(ClientId),
    /// This port's traffic is filtered and distributed to clients using a manager.
    HttpFiltered(HttpFilterManager),
}

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

    /// Mspc sender to clone and give http filter managers so that they can send back requests.
    http_request_sender: Sender<HandlerHttpRequest>,

    /// Receives filtered HTTP requests that need to be forwarded a client.
    http_request_receiver: Receiver<HandlerHttpRequest>,

    /// For informing the [`Self::start`] task about closed connections.
    http_connection_close_sender: Sender<ConnectionId>,

    /// [`Self::start`] listens to this, removes the connection and frees the index.
    http_connection_close_receiver: Receiver<ConnectionId>,

    /// Keep track of the clients that already received an http request out of a connection.
    /// This is used to inform them when the connection is closed.
    ///
    /// Note: The set of clients here is not the same as the set of clients that subscribe to the
    /// port of a connection, as some clients might have defined a regex that no request matched,
    /// so they did not get any request out of this connection, so they are not even aware of this
    /// connection.
    http_connection_clients: HashMap<ConnectionId, HashSet<ClientId>>,

    /// Maps each pending request id to the sender into the channel with the hyper service that
    /// received that requests and is waiting for the response.
    http_response_senders:
        HashMap<(ConnectionId, RequestId), oneshot::Sender<Response<Full<Bytes>>>>,
}

impl TcpConnectionStealer {
    /// Initializes a new [`TcpConnectionStealer`] fields, but doesn't start the actual working
    /// task (call [`TcpConnectionStealer::start`] to do so).
    #[tracing::instrument(level = "trace")]
    pub(crate) async fn new(command_rx: Receiver<StealerCommand>) -> Result<Self, AgentError> {
        let (http_request_sender, http_request_receiver) = channel(1024);
        let (connection_close_sender, connection_close_receiver) = channel(1024);

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
            http_request_sender,
            http_request_receiver,
            http_connection_close_sender: connection_close_sender,
            http_connection_close_receiver: connection_close_receiver,
            http_connection_clients: HashMap::with_capacity(8),
            http_response_senders: HashMap::with_capacity(8),
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
    /// The loop deals with 6 different paths:
    ///
    /// 1. Receiving [`StealerCommand`]s and calling [`TcpConnectionStealer::handle_command`];
    ///
    /// 2. Accepting remote connections through the [`TcpConnectionStealer::stealer`]
    /// [`TcpListener`]. We steal traffic from the created streams.
    ///
    /// 3. Reading incoming data from the stolen remote connections (accepted in 2.) and forwarding
    /// to clients.
    ///
    /// 4. Receiving filtered HTTP requests and forwarding them to clients (layers).
    ///
    /// 5. Receiving the connection IDs of closing filtered HTTP connections, and informing all
    /// clients that were forward a request out of that connection of the closing of that
    /// connection.
    ///
    /// 6. Handling the cancellation of the whole stealer thread.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) async fn start(
        mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), AgentError> {
        loop {
            select! {
                command = self.command_rx.recv() => {
                    if let Some(command) = command {
                        self.handle_command(command).await.map_err(| e | {
                            error!("Failed handling command {e:#?}");
                            e
                        })?;
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
                request = self.http_request_receiver.recv() => self.forward_stolen_http_request(request).await?,
                Some(connection_id) = self.http_connection_close_receiver.recv() => {
                    // Send a close message to all clients that were subscribed to the connection.
                    if let Some(clients) = self.http_connection_clients.remove(&connection_id) {
                        for client_id in clients.into_iter() {
                            if let Some(client_tx) = self.clients.get(&client_id) {
                                client_tx.send(DaemonTcp::Close(TcpClose {connection_id})).await?
                            } else {
                                warn!("Cannot notify client {client_id} on the closing of a connection.")
                            }
                        }
                    }
                    self.index_allocator.free_index(connection_id);
                }

                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    /// Forward a stolen HTTP request from the http filter to the direction of the layer.
    ///
    /// HttpFilter --> Stealer --> Layer --> Local App
    ///                        ^- You are here.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn forward_stolen_http_request(
        &mut self,
        request: Option<HandlerHttpRequest>,
    ) -> Result<(), AgentError> {
        let HandlerHttpRequest {
            request,
            response_tx,
        } = request.ok_or(HttpRequestReceiverClosed)?;

        if let Some(daemon_tx) = self.clients.get(&request.client_id) {
            // Note down: client_id got a request out of connection_id.
            self.http_connection_clients
                .entry(request.connection_id)
                .or_insert_with(|| HashSet::with_capacity(2))
                .insert(request.client_id);
            self.http_response_senders
                .insert((request.connection_id, request.request_id), response_tx);
            Ok(daemon_tx
                .send(DaemonTcp::HttpRequest(request.into_serializable().await?))
                .await?)
        } else {
            warn!(
                "Got stolen request for client {:?} that is not, or no longer, subscribed.",
                request.client_id
            );
            Ok(())
        }
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

        let local_address = stream.local_addr()?.ip();

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
            remote_address: address.ip(),
            local_address,
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
        let mut real_address = orig_dst::orig_dst_addr(&stream)?;
        // If we use the original IP we would go through prerouting and hit a loop.
        // localhost should always work.
        real_address.set_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));
        match self.port_subscriptions.get(&real_address.port()) {
            // We got an incoming connection in a port that is being stolen in its whole by a single
            // client.
            Some(Unfiltered(client_id)) => {
                self.steal_connection(*client_id, address, real_address.port(), stream)
                    .await
            }

            // We got an incoming connection in a port that is being http filtered by one or more
            // clients.
            Some(HttpFiltered(manager)) => {
                let connection_id = self.index_allocator.next_index().unwrap();

                manager
                    .new_connection(
                        stream,
                        real_address,
                        connection_id,
                        self.http_connection_close_sender.clone(),
                    )
                    .await;

                Ok(())
            }

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
        let flush_connections = std::env::var("MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS")
            .ok()
            .and_then(|var| var.parse::<bool>().ok())
            .unwrap_or_default();

        self.iptables = Some(SafeIpTables::new(
            iptables::new(false).unwrap(),
            flush_connections,
        )?);
        Ok(())
    }

    /// Helper function to handle [`Command::PortSubscribe`] messages.
    ///
    /// Inserts `port` into [`TcpConnectionStealer::iptables`] rules, and subscribes the layer with
    /// `client_id` to steal traffic from it.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn port_subscribe(&mut self, client_id: ClientId, port_steal: StealType) -> Result<()> {
        if self.iptables.is_none() {
            // TODO: make the initialization internal to SafeIpTables.
            self.init_iptables()?;
        }
        let mut first_subscriber = false;

        let steal_port = match port_steal {
            StealType::All(port) => match self.port_subscriptions.get(&port) {
                Some(sub) => {
                    error!("Can't steal port {port:?} as it is already being stolen: {sub:?}.");
                    Err(PortAlreadyStolen(port))
                }
                None => {
                    first_subscriber = true;
                    self.port_subscriptions.insert(port, Unfiltered(client_id));
                    Ok(port)
                }
            },
            StealType::FilteredHttp(port, filter) => {
                // Make the regex case-insensitive.
                match Regex::new(&format!("(?i){filter}")) {
                    Ok(regex) => match self.port_subscriptions.get_mut(&port) {
                        Some(Unfiltered(earlier_client)) => {
                            error!("Can't steal port {port:?} as it is already being stolen with no filters by client {earlier_client:?}.");
                            Err(PortAlreadyStolen(port))
                        }
                        Some(HttpFiltered(manager)) => {
                            manager.add_client(client_id, regex);
                            Ok(port)
                        }
                        None => {
                            first_subscriber = true;

                            let manager = HttpFiltered(HttpFilterManager::new(
                                client_id,
                                regex,
                                self.http_request_sender.clone(),
                            ));

                            self.port_subscriptions.insert(port, manager);
                            Ok(port)
                        }
                    },
                    Err(fail) => Err(From::from(BadHttpFilterRegex(filter, fail.to_string()))),
                }
            }
        };

        if first_subscriber && let Ok(port) = steal_port {
            self.iptables()?
                .add_stealer_iptables_rules(port, self.stealer.local_addr()?.port()).await?;
        }

        self.send_message_to_single_client(&client_id, DaemonTcp::SubscribeResult(steal_port))
            .await
    }

    /// Helper function to handle [`Command::PortUnsubscribe`] messages.
    ///
    /// Removes `port` from [`TcpConnectionStealer::iptables`] rules, and unsubscribes the layer
    /// with `client_id`.
    #[tracing::instrument(level = "trace", skip(self))]
    fn port_unsubscribe(&mut self, client_id: ClientId, port: Port) -> Result<(), AgentError> {
        let port_unsubscribed = match self.port_subscriptions.get_mut(&port) {
            Some(HttpFiltered(manager)) => {
                manager.remove_client(&client_id);
                manager.is_empty()
            }
            Some(Unfiltered(subscribed_client)) if *subscribed_client == client_id => true,
            Some(Unfiltered(_)) | None => {
                warn!("A client tried to unsubscribe from a port it was not subscribed to.");
                false
            }
        };
        if port_unsubscribed {
            // No remaining subscribers on this port.
            self.iptables()?
                .remove_stealer_iptables_rules(port, self.stealer.local_addr()?.port())?;

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
            .filter(|(_port, sub)| match sub {
                Unfiltered(port_client) => *port_client == client_id,
                HttpFiltered(manager) => manager.contains_client(&client_id),
            })
            .map(|(port, _sub)| *port)
            .collect()
    }

    /// Removes the client with `client_id` from our list of clients (layers), and also removes
    /// their redirection rules from [`TcpConnectionStealer::iptables`] and all their open
    /// connections.
    #[tracing::instrument(level = "trace", skip(self))]
    fn close_client(&mut self, client_id: ClientId) -> Result<(), AgentError> {
        let ports = self.get_client_ports(client_id);
        for port in ports.iter() {
            self.port_unsubscribe(client_id, *port)?
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

    /// Forward an HTTP response to a stolen HTTP request from the layer back to the HTTP client.
    ///
    ///                         _______________agent_______________
    /// Local App --> Layer --> ClientConnectionHandler --> Stealer --> Browser
    ///                                                             ^- You are here.
    #[tracing::instrument(
        level = "trace",
        skip(self),
        fields(response_senders = ?self.http_response_senders.keys()),
    )]
    async fn http_response(&mut self, response: HttpResponse) {
        match self
            .http_response_senders
            .remove(&(response.connection_id, response.request_id))
        {
            None => {
                error!("Got unexpected http response. Not forwarding.");
            }
            Some(response_tx) => {
                let _res = response // inspecting errors, not propagating.
                    .internal_response
                    .try_into()
                    .inspect_err(|err| {
                        error!("Could not reconstruct http response: {err:?}");
                        debug_assert!(false);
                    })
                    .map(|response| {
                        let _res = response_tx.send(response).inspect_err(|resp| {
                            warn!(
                                "Hyper service has dropped the response receiver before receiving the \
                        response {:?}.",
                                resp
                            );
                        });
                    });
            }
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
            Command::HttpResponse(response) => self.http_response(response).await,
        }

        Ok(())
    }
}

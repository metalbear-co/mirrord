use std::{
    collections::HashSet,
    io,
    net::{Ipv4Addr, SocketAddr},
};

use bytes::Bytes;
use fancy_regex::Regex;
use iptables::IPTables;
use mirrord_protocol::{
    tcp::{NewTcpConnection, TcpClose},
    RemoteError::BadHttpFilterRegex,
    ResponseError::PortAlreadyStolen,
};
use streammap_ext::StreamMap;
use tokio::{
    io::{copy, sink, split, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tracing::error;

use super::{
    http_traffic::{filter::HttpFilter, DefaultReversibleStream},
    *,
};
use crate::{
    steal::{
        http_traffic::{HttpFilterManager, PassthroughRequest},
        StealSubscription::{HttpFiltered, Unfiltered},
    },
    AgentError::{AgentInvariantViolated, HttpRequestReceiverClosed},
};

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
    http_request_sender: Sender<StealerHttpRequest>,

    /// Receives filtered HTTP requests that need to be forwarded a client.
    http_request_receiver: Receiver<StealerHttpRequest>,

    /// For sending letting the [`Self::start`] task this connection was closed.
    http_connection_close_sender: Sender<ConnectionId>,

    /// [`Self::start`] listens to this, removes the connection and frees the index.
    http_connection_close_receiver: Receiver<ConnectionId>,

    /// Used to send http responses back to the original remote connection.
    http_write_streams: HashMap<ConnectionId, WriteHalf<DefaultReversibleStream>>,

    // TODO: ?
    passthrough_sender: Sender<PassthroughRequest>,
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

        let (http_request_sender, http_request_receiver) = channel(1024);
        let (connection_close_sender, connection_close_receiver) = channel(1024);

        // TODO: ?
        let (passthrough_sender, passthrough_receive) = channel(1024);

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
            http_write_streams: HashMap::with_capacity(8),
            passthrough_sender,
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
                request = self.http_request_receiver.recv() => self.forward_stolen_http_request(request).await?,
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
                Some(connection_id) = self.http_connection_close_receiver.recv() => {
                    self.http_write_streams.remove(&connection_id);
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
        request: Option<StealerHttpRequest>,
    ) -> Result<(), AgentError> {
        if let Some(request) = request {
            if let Some(daemon_tx) = self.clients.get(&request.client_id) {
                Ok(daemon_tx
                    .send(DaemonTcp::HttpRequest(request.into_serializable().await?))
                    .await?)
            } else {
                // TODO: can this happen when a client unsubscribes?
                warn!(
                    "Got stolen request for client {:?} that is not, or no longer, subscribed.",
                    request.client_id
                );
                Ok(())
            }
        } else {
            // This shouldn't ever happen, as `self` also holds a sender of that channel.
            Err(HttpRequestReceiverClosed)
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

    /// Create a task that copies all data incoming from [`stream_with_browser`] into
    /// [`stream_with_filter`], drains whatever comes back from [`stream_with_filter`], and hold on
    /// to the writing half of [`stream_with_browser`] in [`self.http_write_streams`], for sending
    /// back responses.
    async fn forward_filter_stream(
        &mut self,
        stream_with_browser: DefaultReversibleStream,
        stream_with_filter: DuplexStream,
        connection_id: ConnectionId,
    ) {
        // In `browser2stealer` the stealer reads incoming data from the outside - from the browser.
        // In `stealer2browser` the stealer writes the responses back to the browser. The responses
        // are either originated in the local application and forwareded by the layer, or if the
        // request was not matched by any filter, the response is originated in the remote app.
        let (mut browser2stealer, stealer2browser) = split(stream_with_browser);

        // `filter2nowhere` is an incoming stream of http responses that the filter (specifically
        // hyper) generates and we ignore.
        // `stealer2filter` is an outgoing stream where we send to the stealer the incoming data
        // from `browser2stealer`.
        let (mut filter2nowhere, mut stealer2filter) = split(stream_with_filter);

        // Hold on to the `stealer2browser` for sending http responses.
        self.http_write_streams
            .insert(connection_id, stealer2browser);

        // Dump all the responses by hyper down a sink.
        // The responses that hyper sends are meaningless, the real responses that will be
        // forwarded come from the layer (or from the deployed application in the case of a
        // passthrough).
        tokio::spawn(async move {
            let mut hyper_response_sink = sink();
            if let Err(err) = copy(&mut filter2nowhere, &mut hyper_response_sink).await {
                error!(
                    "Encountered error: \"{err:?}\" while dumping hyper responses down the sink."
                )
            };
        });

        // With this sender the forwarding task will let the "main task" (`start`) know that the
        // connection is closed and the response stream can be removed.
        let connection_close_sender = self.http_connection_close_sender.clone();

        // Forward all incoming data from browser onwards to the filter.
        tokio::spawn(async move {
            if let Err(err) = copy(&mut browser2stealer, &mut stealer2filter).await {
                error!("Encountered error: \"{err:?}\" while forwarding incoming stream to filter.")
            }
            if let Err(err) = connection_close_sender.send(connection_id).await {
                error!("Stream forwarding task could not communicate stream close to main task.");
            }
        });
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

                if let Some(HttpFilter {
                    reversible_stream,
                    interceptor_stream,
                    ..
                }) = manager
                    .new_connection(stream, real_address, connection_id)
                    .await?
                {
                    self.forward_filter_stream(
                        reversible_stream,
                        interceptor_stream,
                        connection_id,
                    )
                    .await;
                }

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
    async fn port_subscribe(&mut self, client_id: ClientId, port_steal: PortSteal) -> Result<()> {
        if self.iptables.is_none() {
            // TODO: make the initialization internal to SafeIpTables.
            self.init_iptables()?;
        }
        let mut first_subscriber = false;
        let res = match port_steal {
            PortSteal::Steal(port) => {
                if let Some(sub) = self.port_subscriptions.get(&port) {
                    error!(
                        "Can't steal whole port {port:?} as it is already being stolen: {sub:?}."
                    );
                    Err(PortAlreadyStolen(port))
                } else {
                    first_subscriber = true;
                    self.port_subscriptions.insert(port, Unfiltered(client_id));
                    Ok(port)
                }
            }
            PortSteal::HttpFilterSteal(port, regex) => match Regex::new(&regex) {
                Ok(regex) => match self.port_subscriptions.get_mut(&port) {
                    Some(Unfiltered(earlier_client)) => {
                        error!("Can't filter-steal port {port:?} as it is already being stolen in its whole by client {earlier_client:?}.");
                        Err(PortAlreadyStolen(port))
                    }
                    Some(HttpFiltered(manager)) => {
                        manager.new_client(client_id, regex);
                        Ok(port)
                    }
                    None => {
                        first_subscriber = true;
                        let manager = HttpFiltered(HttpFilterManager::new(
                            port,
                            client_id,
                            regex,
                            self.http_request_sender.clone(),
                            self.passthrough_sender.clone(),
                        ));
                        self.port_subscriptions.insert(port, manager);
                        Ok(port)
                    }
                },
                Err(e) => Err(From::from(BadHttpFilterRegex(regex, e.to_string()))),
            },
        };
        if first_subscriber {
            if let Ok(port) = res {
                self.redirect_port(port)?;
            }
        }
        self.send_message_to_single_client(&client_id, DaemonTcp::SubscribeResult(res))
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
            .filter(|(_port, sub)| match sub {
                Unfiltered(port_client) => *port_client == client_id,
                HttpFiltered(manager) => manager.contains_client(&client_id),
            })
            .map(|(port, _sub)| *port)
            .collect()
    }

    /// Removes the client with `client_id` from our list of clients (layers), and also removes
    /// their redirection rules from [`TcpConnectionStealer::iptables`] and all their open
    /// connecitons.
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

    /// Forward an HTTP response to a stolen HTTP request from the layer to the `HttpFilterManager`.
    ///
    ///                       _________________________agent_________________________
    /// Local App -> Layer -> ClientConnectionHandler -> Stealer -> HttpFilterManager -> Browser
    ///                                                          ^- You are here.
    async fn http_response(
        &mut self,
        response: HttpResponse,
    ) -> std::result::Result<(), AgentError> {
        todo!();
        match self.port_subscriptions.get(&response.port) {
            Some(HttpFiltered(manager)) => todo!(),
            Some(Unfiltered(_)) | None => {
                warn!(
                    "Got an http response to port {:?} which is not being filtered. \
                Maybe the layer unsubscribed and this is a remainder of a closed connection. \
                Not forwarding.",
                    response.port
                );
                Ok(())
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
            Command::HttpResponse(response) => self.http_response(response).await?,
        }

        Ok(())
    }
}

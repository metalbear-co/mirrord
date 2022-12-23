use std::{
    collections::{BinaryHeap, HashSet},
    io,
    net::{Ipv4Addr, SocketAddr},
};

use bytes::{BufMut, Bytes, BytesMut};
use fancy_regex::Regex;
use http_body_util::Full;
use hyper::{body::Body, Response, Version};
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
    error::Result,
    steal::{
        http_traffic::{HttpFilterManager, UnmatchedResponse},
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

    /// For a connection with pending requests, a priority queue with responses that were already
    /// returned, but that cannot be sent yet because there are earlier responses that are not
    /// available yet.
    http_response_queues: HashMap<ConnectionId, BinaryHeap<HttpResponse>>,

    /// Keep track of the clients that already received an http request out of a connection.
    /// This is used to inform them when the connection is closed.
    ///
    /// Note: The set of clients here is not the same as the set of clients that subscribe to the
    /// port of a connection, as some clients might have defined a regex that no request matched,
    /// so they did not get any request out of this connection, so they are not even aware of this
    /// connection.
    http_connection_clients: HashMap<ConnectionId, HashSet<ClientId>>,

    /// Saves for a connection the request_id of the next response that should be sent.
    http_request_counters: HashMap<ConnectionId, RequestId>,

    /// Send this channel to the [`HyperHandler`], where it's used to handle the unmatched HTTP
    /// requests case (when no HTTP filter matches a request).
    unmatched_tx: Sender<UnmatchedResponse>,

    /// Channel that receives responses which did not match any HTTP filter.
    unmatched_rx: Receiver<UnmatchedResponse>,
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
        let (unmatched_tx, unmatched_rx) = channel(1024);

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
            http_response_queues: HashMap::with_capacity(8),
            http_connection_clients: HashMap::with_capacity(8),
            http_request_counters: HashMap::with_capacity(8),
            unmatched_tx,
            unmatched_rx,
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
                    // Send a close message to all clients that were subscribed to the connection.
                    if let Some(clients) = self.http_connection_clients.remove(&connection_id) {
                        // TODO: is this too much to do in an iteration of the select loop?
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

                // Handles the responses that were not captured by any HTTP filter.
                Some(UnmatchedResponse(response)) = self.unmatched_rx.recv() => {
                    // TODO(alex) [high] 2022-12-22: Convert `response` into bytes.
                    // Send the bytes to `self.connections.get(connection_id)` stream.
                    //
                    // This should probably be put into the same list of the normal requests queue,
                    // instead of sending directly on a stream.
                    todo!()
                }

                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    // TODO(alex) [high] 2022-12-22: Convert `response` into bytes.
    fn foo(UnmatchedResponse(response): UnmatchedResponse) {
        let b: Vec<u8> = response.try_into().unwrap();
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
                // Note down: client_id got a request out of connection_id.
                self.http_connection_clients
                    .entry(request.connection_id)
                    .or_insert_with(|| HashSet::with_capacity(2))
                    .insert(request.client_id);
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
        // are either originated in the local application and forwarded by the layer, or if the
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
                error!("Stream forwarding task could not communicate stream close to main task: {err:?}.");
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
    async fn port_subscribe(&mut self, client_id: ClientId, port_steal: StealType) -> Result<()> {
        if self.iptables.is_none() {
            // TODO: make the initialization internal to SafeIpTables.
            self.init_iptables()?;
        }
        let mut first_subscriber = false;
        let res = match port_steal {
            StealType::All(port) => {
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
            StealType::FilteredHttp(port, regex) => match Regex::new(&regex) {
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
                            self.unmatched_tx.clone(),
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

    // TODO: make this correct.
    async fn response_to_bytes(response: Response<Full<Bytes>>) -> Vec<u8> {
        let (
            response::Parts {
                status,
                version,
                headers,
                extensions: _, // TODO: should we be using this?
                ..
            },
            body,
        ) = response.into_parts();

        // Enough for body + headers of length 64 each + 64 bytes for status line.
        // This will probably be more than enough most of the time.
        let mut bytes =
            BytesMut::with_capacity(body.size_hint().lower() as usize + 64 * headers.len() + 64);

        let version = match version {
            Version::HTTP_09 => "HTTP/0.9",
            Version::HTTP_10 => "HTTP/1.0",
            Version::HTTP_11 => "HTTP/1.1",
            Version::HTTP_2 => "HTTP/2",
            Version::HTTP_3 => "HTTP/3",
            _ => "HTTP/1.1", // TODO: What should happen here?
        };

        // Status line.
        bytes.put(version.as_bytes()); // HTTP/1.1
        bytes.put(&b" "[..]);
        bytes.put(status.as_str().as_bytes()); // 200
        bytes.put(&b" "[..]);
        if let Some(reason) = status.canonical_reason() {
            bytes.put(reason.as_bytes()); // OK
        } else {
            bytes.put(&b"LOL"[..]);
        }

        // Headers.
        for (name, value) in headers.into_iter() {
            if let Some(name) = name {
                bytes.put(&b"\r\n"[..]);
                bytes.put(name.as_str().as_bytes());
                bytes.put(&b": "[..]);
            } else {
                // TODO: are multiple values for the same header comma delimited?
                bytes.put(&b","[..])
            }
            bytes.put(value.as_bytes())
        }
        bytes.put(&b"\r\n\r\n"[..]);

        // Body.
        bytes.put(body.collect().await.unwrap().to_bytes()); // Unwrap Infallible.

        bytes.to_vec()
    }

    fn handle_response_reconstruction_fail(err: hyper::http::Error) -> hyper::http::Error {
        error!("Could not reconstruct http response: {err:?}");
        // TODO: What should happen? Currently just skipping that response, which would
        //       also spoils the rest of the stream. Should hopefully never happen.
        debug_assert!(false);
        err
    }

    /// Forward an HTTP response to a stolen HTTP request from the layer to the back to the browser.
    ///
    ///                         _______________agent_______________
    /// Local App --> Layer --> ClientConnectionHandler --> Stealer --> Browser
    ///                                                             ^- You are here.
    async fn http_response(&mut self, response: HttpResponse) -> Result<()> {
        let http_tx = if let Some(stream) = self.http_write_streams.get_mut(&response.connection_id)
        {
            stream
        } else {
            warn!(
                "Got an http response in a connection for which no tcp stream is present. Not forwarding.",
            );
            return Ok(());
        };
        let counter = self
            .http_request_counters
            .entry(response.connection_id)
            .or_insert(0);
        if response.request_id == *counter {
            if let Ok(response) = response
                .response
                .try_into()
                .map_err(Self::handle_response_reconstruction_fail)
            {
                http_tx
                    .write_all(&Self::response_to_bytes(response).await)
                    .await?; // TODO: handle error?
            }
            *counter += 1;
            if let Some(queue) = self.http_response_queues.get_mut(&response.connection_id) {
                while queue
                    .peek()
                    .is_some_and(|response| response.request_id == *counter)
                {
                    // The next response arrived before the one that just arrived and was waiting
                    // in the queue for all earlier responses to be sent.
                    let response = queue.pop().unwrap();
                    if let Ok(response) = response
                        .response
                        .try_into()
                        .map_err(Self::handle_response_reconstruction_fail)
                    {
                        http_tx
                            .write_all(&Self::response_to_bytes(response).await)
                            .await?; // TODO: handle error?
                    }
                    *counter += 1;
                }
            }
        } else {
            // The response is not the next one that should be sent back, so store in the priority
            // queue until all the earlier responses are available.

            if response.request_id > *counter {
                self.http_response_queues
                    .entry(response.connection_id)
                    .or_insert_with(|| BinaryHeap::with_capacity(8))
                    .push(response);
            } else {
                error!(
                    "Got an http response with a request_id that was already received and sent to \
                    the browser for this connection. Dropping the duplicate response."
                );
            }
        }
        Ok(())
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

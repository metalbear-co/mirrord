use std::{
    collections::VecDeque,
    error::Report,
    fmt,
    ops::{ControlFlow, Not},
    sync::Arc,
    time::Duration,
};

use futures::{SinkExt, StreamExt, stream::SelectAll};
use mirrord_protocol::{CLIENT_READY_FOR_LOGS, ClientMessage, DaemonMessage, LogMessage};
use tokio::{
    sync::mpsc,
    time::{self, Instant},
};
use tokio_retry::strategy::ExponentialBackoff;

use crate::{
    client::{
        config::ClientConfig,
        connector::ProtocolConnector,
        error::{ClientError, TaskError, TaskResult},
        incoming::{Incoming, IncomingMode},
        outbox::OutBox,
        outgoing::Outgoing,
        ping_pong::PingPong,
        queue_kind::{QueueKind, QueueKindMap},
        request::{ClientRequest, ClientRequestStream},
        simple_request::ResultHandler,
        tunnels::TrafficTunnels,
    },
    fifo::{FifoClosedError, FifoSink},
    file_ext::{FileRequestExt, FileResponseExt},
    id_tracker::IdTracker,
    shrinkable::Shrinkable,
    timeout,
};

/// Background task that handles [`mirrord_protocol`] connection(s) with a server
/// on behalf of multiple [`MirrordClient`](crate::client::MirrordClient) instance.
pub struct ClientTask<C: ProtocolConnector> {
    /// Used for reconnecting.
    connector: C,
    /// Established connection.
    connection: C::Conn,
    /// Negotiated [`mirrord_protocol`] version.
    ///
    /// This version will remain constant across all reconnects.
    protocol_version: semver::Version,

    /// Requests coming from [`MirrordClient`](crate::client::MirrordClient) instances.
    client_requests: SelectAll<ClientRequestStream>,
    /// For receiving new [`ClientRequest`] streams.
    new_clients_rx: mpsc::Receiver<ClientRequestStream>,

    /// For sending [`LogMessage`]s to the [`MirrordClient`](crate::client::MirrordClient)
    /// that issued the latest [`ClientRequest::Logs`].
    ///
    /// Cleared each time we lose the connection.
    log_tx: Option<FifoSink<LogMessage>>,
    /// For tracking ping pong state.
    ///
    /// Reset each time we lose the connection.
    ping_pong: PingPong,
    /// For handling outgoing traffic feature.
    ///
    /// Cleared each time we lose the connection.
    outgoing: Outgoing,
    /// For handling incoming traffic feature.
    ///
    /// Cleared each time we lose the connection.
    incoming: Incoming,
    /// For remapping remote file descriptors in [`ClientMessage::FileRequest`]s and
    /// [`DaemonMessage::File`]s.
    ///
    /// Adjusted each time we lose the connection.
    fd_tracker: IdTracker,
    /// For handling responses to simple requests, for example file ops and DNS resolutions.
    ///
    /// Cleared each time we lose the connection.
    queues: QueueKindMap<VecDeque<Box<dyn ResultHandler>>>,
    /// For handling all tunneled traffic (both outgoing and incoming feature).
    ///
    /// Cleared each time we lose the connection.
    tunnels: TrafficTunnels,
    /// [`ClientConfig::server_send_timeout`].
    server_send_timeout: Duration,
    /// [`ClientConfig::logs_fifo_timeout`]
    logs_fifo_timeout: Duration,
}

impl<C: ProtocolConnector> ClientTask<C> {
    pub async fn new(
        mut connector: C,
        new_clients_rx: mpsc::Receiver<ClientRequestStream>,
        config: ClientConfig,
    ) -> TaskResult<Self> {
        let (connection, protocol_version) =
            Self::connect(&mut connector, None, config.server_send_timeout).await?;
        let outgoing = Outgoing::new(&protocol_version);
        let incoming = Incoming::new(
            config.subscription_fifo_capacity,
            config.subscription_fifo_timeout,
        );
        let tunnels = TrafficTunnels::new(
            config.tunnel_fifo_capacity,
            config.tunnel_fifo_timeout,
            &protocol_version,
        );

        let this = Self {
            connector,
            connection,
            protocol_version,
            client_requests: Default::default(),
            new_clients_rx,
            log_tx: None,
            ping_pong: PingPong::new(config.ping_pong_interval),
            outgoing,
            incoming,
            fd_tracker: IdTracker::new(config.initial_fd_offset),
            queues: Default::default(),
            tunnels,
            server_send_timeout: config.server_send_timeout,
            logs_fifo_timeout: config.logs_fifo_timeout,
        };

        Ok(this)
    }

    pub async fn run(mut self) -> TaskResult<()> {
        loop {
            let error = match self.tick().await {
                Ok(ControlFlow::Continue(())) => continue,
                Ok(ControlFlow::Break(())) => break,
                Err(error) => error,
            };

            if error.can_reconnect().not() || self.connector.can_reconnect().not() {
                tracing::warn!(
                    error = %Report::new(&error),
                    client_state = ?self,
                    "mirrord-protocol client encountered an error, reconnect not possible",
                );
                return Err(error);
            }

            tracing::warn!(
                error = %Report::new(&error),
                client_state = ?self,
                "mirrord-protocol client encountered an error, reconnecting",
            );
            self.outgoing.server_connection_lost(&error);
            self.incoming.server_connection_lost(&error);
            self.tunnels.server_connection_lost();
            self.ping_pong.server_connection_lost();
            self.fd_tracker.server_connection_lost();
            std::mem::take(&mut self.queues)
                .into_values()
                .into_iter()
                .flatten()
                .for_each(|handler| {
                    handler(Err(ClientError::ConnectionLost(error.clone()))).ok();
                });
            self.log_tx = None;

            let started_at = Instant::now();
            let (new_conn, _) = Self::connect(
                &mut self.connector,
                Some(&self.protocol_version),
                self.server_send_timeout,
            )
            .await
            .inspect_err(|error| {
                tracing::warn!(
                    error = %Report::new(error),
                    client_state = ?self,
                    elapsed = ?started_at.elapsed(),
                    "mirrord-protocol client failed to reconnect",
                )
            })
            .inspect(|_| {
                tracing::info!(
                    error = %Report::new(&error),
                    elapsed = ?started_at.elapsed(),
                    "mirrord-protocol client reconnected",
                );
            })?;
            self.connection = new_conn;
        }

        timeout::rich_timeout(self.server_send_timeout, self.connection.close())
            .await
            .ok();

        Ok(())
    }

    async fn tick(&mut self) -> TaskResult<ControlFlow<()>> {
        let out = tokio::select! {
            new_stream = self.new_clients_rx.recv() => {
                let Some(new_stream) = new_stream else {
                    return Ok(ControlFlow::Break(()));
                };
                self.client_requests.push(new_stream);
                OutBox::default()
            },
            Some(message) = self.client_requests.next() => self.handle_client_request(message),
            result = self.ping_pong.tick() => {
                result?;
                OutBox::from(ClientMessage::Ping)
            },
            Some(out) = self.incoming.next() => out,
            Some(out) = self.tunnels.next() => out,
            message = self.connection.next() => self.handle_server_message(message).await?,
        };

        for message in out {
            timeout::rich_timeout(self.server_send_timeout, self.connection.feed(message))
                .await?
                .map_err(TaskError::io)?;
        }

        timeout::rich_timeout(self.server_send_timeout, self.connection.flush())
            .await?
            .map_err(TaskError::io)?;

        Ok(ControlFlow::Continue(()))
    }

    pub fn protocol_version(&self) -> &semver::Version {
        &self.protocol_version
    }

    /// Handles a request from one of the connected
    /// [`MirrordClient`](crate::client::MirrordClient)s.
    fn handle_client_request(&mut self, request: ClientRequest) -> OutBox {
        match request {
            ClientRequest::Simple {
                mut message,
                queue_kind,
                handler,
            } => {
                let remote_fd = match &mut message {
                    ClientMessage::FileRequest(req) => req.remote_fd_mut(),
                    _ => None,
                };
                if let Some(fd) = remote_fd {
                    let Some(new_fd) = self.fd_tracker.map_id_from_client(*fd) else {
                        handler(Err(ClientError::LostFileDescriptor(*fd))).ok();
                        return Default::default();
                    };
                    *fd = new_fd;
                };
                self.queues[queue_kind].push_back(handler);
                message.into()
            }

            ClientRequest::SimpleNoResponse(mut message) => {
                let remote_fd = match &mut message {
                    ClientMessage::FileRequest(req) => req.remote_fd_mut(),
                    _ => None,
                };
                if let Some(fd) = remote_fd {
                    let Some(new_fd) = self.fd_tracker.map_id_from_client(*fd) else {
                        return Default::default();
                    };
                    *fd = new_fd;
                };
                message.into()
            }

            ClientRequest::ConnectIp {
                addr,
                mode,
                response_tx,
            } => self.outgoing.handle_connect_ip(addr, mode, response_tx),

            ClientRequest::ConnectUnix { addr, response_tx } => {
                self.outgoing.handle_connect_unix(addr, response_tx)
            }

            ClientRequest::SubscribePort {
                port,
                mode,
                filter,
                response_tx,
            } => self
                .incoming
                .handle_subscribe(mode, port, filter, response_tx),

            ClientRequest::Logs(tx) => (self.log_tx.replace(tx).is_none()
                && CLIENT_READY_FOR_LOGS.matches(&self.protocol_version))
            .then_some(ClientMessage::ReadyForLogs)
            .map(OutBox::from)
            .unwrap_or_default(),
        }
    }

    /// Handles a received [`mirrord_protocol`] message result.
    async fn handle_server_message(
        &mut self,
        message: Option<Result<DaemonMessage, C::Error>>,
    ) -> TaskResult<OutBox> {
        let message = message
            .ok_or(TaskError::ServerClosed(None))?
            .map_err(TaskError::io)?;

        let is_pong = matches!(&message, DaemonMessage::Pong);
        self.ping_pong.got_message(is_pong)?;

        let out = match message {
            // Handled above.
            DaemonMessage::Pong => Default::default(),
            DaemonMessage::LogMessage(log) => {
                if let Some(tx) = self.log_tx.as_mut() {
                    let result = time::timeout(self.logs_fifo_timeout, tx.send(log)).await;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(FifoClosedError)) => self.log_tx = None,
                        Err(_elapsed) => {
                            tx.take_pending();
                        }
                    }
                }
                Default::default()
            }
            DaemonMessage::Close(reason) => {
                return Err(TaskError::ServerClosed(Some(reason)));
            }
            DaemonMessage::Tcp(message) => {
                self.incoming
                    .handle_server_message(IncomingMode::Mirror, message, &mut self.tunnels)
                    .await?
            }
            DaemonMessage::TcpSteal(message) => {
                self.incoming
                    .handle_server_message(IncomingMode::Steal, message, &mut self.tunnels)
                    .await?
            }
            DaemonMessage::TcpOutgoing(message) => {
                self.outgoing
                    .handle_server_message_tcp(message, &mut self.tunnels)
                    .await?
            }
            DaemonMessage::UdpOutgoing(message) => {
                self.outgoing
                    .handle_server_message_udp(message, &mut self.tunnels)
                    .await?
            }
            DaemonMessage::OperatorPing(id) => ClientMessage::OperatorPong(id).into(),
            mut message @ (DaemonMessage::GetAddrInfoResponse(..)
            | DaemonMessage::GetEnvVarsResponse(..)
            | DaemonMessage::ReverseDnsLookup(..)
            | DaemonMessage::File(..)) => {
                let queue_kind = match &message {
                    DaemonMessage::GetAddrInfoResponse(..) => QueueKind::Dns,
                    DaemonMessage::GetEnvVarsResponse(..) => QueueKind::EnvVars,
                    DaemonMessage::ReverseDnsLookup(..) => QueueKind::ReverseDns,
                    DaemonMessage::File(..) => QueueKind::Files,
                    _ => unreachable!(),
                };
                let queue = &mut self.queues[queue_kind];
                let Some(handler) = queue.pop_front() else {
                    return Err(TaskError::unexpected_message(&message));
                };
                queue.smart_shrink();
                if let Some(fd) = message.remote_fd_mut() {
                    let new_fd = self.fd_tracker.map_id_from_server(*fd);
                    *fd = new_fd;
                }
                handler(Ok(message))?;
                Default::default()
            }
            message @ (DaemonMessage::SwitchProtocolVersionResponse(..)
            | DaemonMessage::Vpn(..)
            | DaemonMessage::PauseTarget(..)) => {
                return Err(TaskError::unexpected_message(&message));
            }
        };

        Ok(out)
    }

    /// Reconnects to the mirrord-protocol server.
    ///
    /// Makes at most 8 attempts over ~10s.
    ///
    /// The returned connection is already initialized with [`Self::init_connection`].
    async fn connect(
        connector: &mut C,
        require_version: Option<&semver::Version>,
        server_send_timeout: Duration,
    ) -> TaskResult<(C::Conn, semver::Version)> {
        let started_at = Instant::now();
        let mut backoff = ExponentialBackoff::from_millis(2)
            .factor(125)
            .max_delay(Duration::from_secs(2))
            .take(7);

        loop {
            let result: TaskResult<(C::Conn, semver::Version)> = try {
                let mut conn = connector
                    .connect()
                    .await
                    .inspect_err(|error| {
                        tracing::warn!(
                            error = %Report::new(error),
                            time_in_reconnect = ?started_at.elapsed(),
                            "Failed to connect to the mirrord-protocol server.",
                        )
                    })
                    .map_err(TaskError::io)?;
                let version =
                    Self::init_connection(&mut conn, require_version, server_send_timeout)
                        .await
                        .inspect_err(|error| {
                            tracing::warn!(
                                error = %Report::new(error),
                                time_in_reconnect = ?started_at.elapsed(),
                                "Failed to initialize a new mirrord-protocol server connection.",
                            );
                        })?;
                (conn, version)
            };

            match result {
                Ok(values) => break Ok(values),
                Err(error) => {
                    let backoff = (connector.can_reconnect() && error.can_reconnect())
                        .then(|| backoff.next())
                        .flatten();
                    let Some(backoff) = backoff else {
                        break Err(error);
                    };
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    /// Initializes a new [`mirrord_protocol`] connection, negotiating version of the protocol.
    async fn init_connection(
        conn: &mut C::Conn,
        require_version: Option<&semver::Version>,
        server_send_timeout: Duration,
    ) -> TaskResult<semver::Version> {
        timeout::rich_timeout(
            server_send_timeout,
            conn.send(ClientMessage::SwitchProtocolVersion(
                require_version
                    .unwrap_or(&*mirrord_protocol::VERSION)
                    .clone(),
            )),
        )
        .await?
        .map_err(TaskError::io)?;

        let version = loop {
            let message = timeout::rich_timeout(server_send_timeout, conn.next())
                .await?
                .ok_or(TaskError::ServerClosed(None))?
                .map_err(TaskError::io)?;

            match message {
                DaemonMessage::SwitchProtocolVersionResponse(version) => break version,
                DaemonMessage::Close(reason) => {
                    return Err(TaskError::ServerClosed(Some(reason)));
                }
                DaemonMessage::LogMessage(log) => {
                    tracing::warn!(
                        level = ?log.level,
                        message = log.message,
                        "mirrord-protocol server sent a log message \
                        before the client opted in for logs"
                    );
                }
                DaemonMessage::OperatorPing(id) => {
                    timeout::rich_timeout(
                        server_send_timeout,
                        conn.send(ClientMessage::OperatorPong(id)),
                    )
                    .await?
                    .map_err(TaskError::io)?;
                }
                message @ (DaemonMessage::Tcp(_)
                | DaemonMessage::TcpSteal(_)
                | DaemonMessage::TcpOutgoing(_)
                | DaemonMessage::UdpOutgoing(_)
                | DaemonMessage::File(_)
                | DaemonMessage::GetEnvVarsResponse(_)
                | DaemonMessage::GetAddrInfoResponse(_)
                | DaemonMessage::PauseTarget(_)
                | DaemonMessage::Vpn(_)
                | DaemonMessage::ReverseDnsLookup(_)
                | DaemonMessage::Pong) => return Err(TaskError::unexpected_message(&message)),
            }
        };

        if let Some(require_version) = require_version
            && *require_version != version
        {
            return Err(TaskError::ReconnectedWithDowngradedProtocol(Arc::new((
                require_version.clone(),
                version,
            ))));
        }

        Ok(version)
    }
}

impl<C: ProtocolConnector> fmt::Debug for ClientTask<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            connector,
            protocol_version,
            client_requests,
            new_clients_rx,
            log_tx,
            ping_pong,
            outgoing,
            incoming,
            fd_tracker,
            queues,
            tunnels,
            server_send_timeout,
            ..
        } = self;

        f.debug_struct("ClientTask")
            .field("connector", connector)
            .field("protocol_version", protocol_version)
            .field("client_queues", &client_requests.len())
            .field("new_client_queues_rx_closed", &new_clients_rx.is_closed())
            .field("new_clients_queues_rx_len", &new_clients_rx.len())
            .field("log_tx", log_tx)
            .field("ping_pong", ping_pong)
            .field("outgoing", outgoing)
            .field("incoming", incoming)
            .field("fd_tracker", fd_tracker)
            .field("queues", &QueuesDebug(queues))
            .field("tunnels", tunnels)
            .field("server_send_timeout", server_send_timeout)
            .finish()
    }
}

struct QueuesDebug<'a, T>(&'a QueueKindMap<VecDeque<T>>);

impl<T> fmt::Debug for QueuesDebug<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut m = f.debug_map();
        for (kind, queue) in self.0.iter() {
            m.entry(kind, &queue.len());
        }
        m.finish()
    }
}

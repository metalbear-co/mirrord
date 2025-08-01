use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    fmt,
    time::Duration,
};

use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, ExecutionKind, Reporter};
use mirrord_config::{config::ConfigContext, LayerConfig};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{
    tcp::{
        ChunkedRequest, DaemonTcp, HttpRequestMetadata, IncomingTrafficTransportType,
        InternalHttpBodyFrame, InternalHttpRequest, LayerTcp, NewTcpConnectionV1,
        NewTcpConnectionV2, TcpData,
    },
    ClientMessage, ConnectionId, DaemonMessage, LogLevel, LogMessage, RequestId, ResponseError,
};
use thiserror::Error;
use tokio::{
    sync::mpsc,
    time::{Interval, MissedTickBehavior},
};
use tracing::{debug, info};

use super::config::DumpArgs;
use crate::{
    connection::{create_and_connect, AgentConnection},
    error::CliResult,
    user_data::UserData,
};

/// Implements the `mirrord dump` command.
///
/// This command:
/// 1. Starts a mirrord session using the given config file and target arguments
/// 2. Subscribes to mirror traffic from the specified ports
/// 3. Prints all incoming traffic to stdout in a human friendly format
pub async fn dump_command(
    args: &DumpArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    // Set up configuration similar to exec command
    let mut cfg_context = ConfigContext::default().override_envs(args.params.as_env_vars());

    let mut config = LayerConfig::resolve(&mut cfg_context)?;

    let mut progress = ProgressTracker::from_env("mirrord dump");
    let mut analytics = AnalyticsReporter::new(
        config.telemetry,
        ExecutionKind::Dump,
        watch,
        user_data.machine_id(),
    );

    if !args.params.disable_version_check {
        super::prompt_outdated_version(&progress).await;
    }
    // Collect analytics
    (&config).collect_analytics(analytics.get_mut());

    // Create connection to the agent
    let (_connection_info, connection) =
        create_and_connect(&mut config, &mut progress, &mut analytics, None).await?;

    // Start the dump session
    let session = DumpSession::new(connection, args.ports.clone());
    session.run(&mut progress).await?;

    Ok(())
}

/// Errors that can occur when dumping incoming traffic with `mirrord dump`.
#[derive(Debug, Error)]
pub enum DumpSessionError {
    #[error("agent connection was closed: {}", .0.as_deref().unwrap_or("<no close message>"))]
    AgentConnClosed(Option<String>),

    #[error("received an unexpected message from the agent: {0:?}")]
    UnexpectedAgentMessage(DaemonMessage),

    #[error("port subscription failed: {0}")]
    PortSubscriptionFailed(ResponseError),
}

impl From<mpsc::error::SendError<ClientMessage>> for DumpSessionError {
    fn from(_: mpsc::error::SendError<ClientMessage>) -> Self {
        Self::AgentConnClosed(None)
    }
}

/// Implements `mirrord dump` logic on an established [`AgentConnection`].
struct DumpSession {
    connection: AgentConnection,
    ports: Vec<u16>,
    /// How many of our port subscriptions were confirmed.
    confirmations: usize,
    /// Determines when to send the next [`ClientMessage::Ping`].
    ping_interval: Interval,
    /// We queue incoming traffic until all port subscriptions are confirmed.
    ///
    /// This is requried in order not to lose any traffic and still keep progress output nice.
    queued_messages: Vec<DaemonTcp>,
    /// Maps connection id to request ids.
    ///
    /// Used when handling [`DaemonTcp::Close`].
    conn_id_to_req_id: HashMap<ConnectionId, HashSet<RequestId>>,
}

impl DumpSession {
    fn new(connection: AgentConnection, ports: Vec<u16>) -> Self {
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Self {
            connection,
            ports,
            confirmations: Default::default(),
            ping_interval,
            queued_messages: Default::default(),
            conn_id_to_req_id: Default::default(),
        }
    }

    /// Initializes connection with the agent.
    ///
    /// 1. Negotiates [`mirrord_protocol`] version.
    /// 2. Signals readiness for logs.
    /// 3. Issues port subscriptions.
    async fn init_connection(&mut self) -> Result<(), DumpSessionError> {
        self.connection
            .sender
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await?;
        match self
            .connection
            .receiver
            .recv()
            .await
            .ok_or(DumpSessionError::AgentConnClosed(None))?
        {
            DaemonMessage::SwitchProtocolVersionResponse(version) => {
                debug!("Established mirrord-protocol version {version}");
            }
            other => return Err(DumpSessionError::UnexpectedAgentMessage(other)),
        }
        self.connection
            .sender
            .send(ClientMessage::ReadyForLogs)
            .await?;

        for port in &self.ports {
            let message = ClientMessage::Tcp(LayerTcp::PortSubscribe(*port));
            self.connection.sender.send(message).await?;
            info!("Issued subscription to port {} for mirroring", port);
        }

        Ok(())
    }

    fn handle_tcp_message(
        &mut self,
        message: DaemonTcp,
        progress: &mut ProgressTracker,
    ) -> Result<(), DumpSessionError> {
        if self.confirmations < self.ports.len() {
            match message {
                DaemonTcp::SubscribeResult(Ok(..)) => {
                    self.confirmations += 1;
                    if self.confirmations == self.ports.len() {
                        tracing::debug!("All subscriptions confirmed");
                        progress.info("Listening for traffic... Press Ctrl+C to stop");
                        progress.success(Some("Subscribed to all ports"));
                        for message in std::mem::take(&mut self.queued_messages) {
                            self.handle_tcp_message(message, progress)?;
                        }
                    }
                }
                DaemonTcp::SubscribeResult(Err(error)) => {
                    return Err(DumpSessionError::PortSubscriptionFailed(error));
                }
                other => {
                    tracing::debug!("Queueing message until all subscriptions are confirmed");
                    self.queued_messages.push(other);
                }
            }

            return Ok(());
        }

        match message {
            DaemonTcp::Close(close) => match self.conn_id_to_req_id.remove(&close.connection_id) {
                Some(request_ids) => {
                    for request_id in request_ids {
                        println!(
                            "## Request ID [{}:{}] finished",
                            close.connection_id, request_id
                        );
                    }
                }
                None => {
                    println!("## Connection ID {} closed", close.connection_id);
                }
            },
            DaemonTcp::Data(TcpData {
                connection_id,
                bytes,
            }) => {
                println!("## Connection ID {connection_id}: {} bytes", bytes.len());
                if !bytes.is_empty() {
                    // Try to print as string, fallback to hex if not valid UTF-8
                    match std::str::from_utf8(&bytes) {
                        Ok(s) => println!("Data:\n{}", s),
                        Err(..) => println!("Data (hex):\n{}", hex::encode(bytes.as_ref())),
                    }
                }
            }
            DaemonTcp::HttpRequest(req) => {
                self.conn_id_to_req_id
                    .entry(req.connection_id)
                    .or_default()
                    .insert(req.request_id);
                println!(
                    "## New HTTP request received: Request ID [{}:{}] to port {}",
                    req.connection_id, req.request_id, req.port,
                );
                println!("{}", RequestHead(&req.internal_request));
                println!(
                    "## Request [{}:{}] body: {} bytes",
                    req.connection_id,
                    req.request_id,
                    req.internal_request.body.len(),
                );
                match std::str::from_utf8(&req.internal_request.body) {
                    Ok(s) => println!("{s}"),
                    Err(..) => {
                        println!("{}\n(hex)", hex::encode(req.internal_request.body.as_ref()));
                    }
                }
            }
            DaemonTcp::HttpRequestFramed(req) => {
                self.conn_id_to_req_id
                    .entry(req.connection_id)
                    .or_default()
                    .insert(req.request_id);
                println!(
                    "## New HTTP request received: Request ID [{}:{}] to port {}",
                    req.connection_id, req.request_id, req.port,
                );
                println!("{}", RequestHead(&req.internal_request));
                for frame in req.internal_request.body.0 {
                    println!(
                        "{}",
                        RequestFrame {
                            connection_id: req.connection_id,
                            request_id: req.request_id,
                            frame
                        }
                    );
                }
            }
            DaemonTcp::HttpRequestChunked(chunked) => match chunked {
                ChunkedRequest::StartV1(req) => {
                    self.conn_id_to_req_id
                        .entry(req.connection_id)
                        .or_default()
                        .insert(req.request_id);
                    println!(
                        "## New HTTP request received: Request ID [{}:{}] to port {}",
                        req.connection_id, req.request_id, req.port,
                    );
                    println!("{}", RequestHead(&req.internal_request));
                    for frame in req.internal_request.body {
                        println!(
                            "{}",
                            RequestFrame {
                                connection_id: req.connection_id,
                                request_id: req.request_id,
                                frame
                            }
                        );
                    }
                }
                ChunkedRequest::StartV2(req) => {
                    self.conn_id_to_req_id
                        .entry(req.connection_id)
                        .or_default()
                        .insert(req.request_id);
                    let HttpRequestMetadata::V1 {
                        source,
                        destination,
                    } = &req.metadata;
                    println!(
                        "## New {} request received: Request ID [{}:{}] from {source} to {destination}",
                        match &req.transport {
                            IncomingTrafficTransportType::Tcp => "HTTP".to_string(),
                            IncomingTrafficTransportType::Tls { alpn_protocol, server_name } => format!(
                                "HTTPS (ALPN={:?}, SNI={server_name:?})",
                                alpn_protocol.as_deref().map(String::from_utf8_lossy),
                            )
                        },
                        req.connection_id,
                        req.request_id,
                    );
                    println!("{}", RequestHead(&req.request));
                    for frame in req.request.body.frames {
                        println!(
                            "{}",
                            RequestFrame {
                                connection_id: req.connection_id,
                                request_id: req.request_id,
                                frame
                            }
                        );
                    }
                }
                ChunkedRequest::Body(body) => {
                    for frame in body.frames {
                        println!(
                            "{}",
                            RequestFrame {
                                connection_id: body.connection_id,
                                request_id: body.request_id,
                                frame
                            }
                        );
                    }
                }
                ChunkedRequest::ErrorV1(error) => {
                    println!(
                        "## Request ID [{}:{}] failed",
                        error.connection_id, error.request_id
                    );
                }
                ChunkedRequest::ErrorV2(error) => {
                    println!(
                        "## Request ID [{}:{}] failed: {}",
                        error.connection_id, error.request_id, error.error_message
                    );
                }
            },
            DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                connection_id,
                remote_address,
                destination_port,
                source_port,
                local_address,
            }) => {
                println!(
                    "## New TCP connection established: Connection ID {connection_id} \
                    from {remote_address}:{source_port} to {local_address}:{destination_port}",
                );
            }
            DaemonTcp::NewConnectionV2(NewTcpConnectionV2 {
                connection:
                    NewTcpConnectionV1 {
                        connection_id,
                        remote_address,
                        destination_port,
                        source_port,
                        local_address,
                    },
                transport,
            }) => {
                println!(
                    "## New {} connection established: Connection ID {connection_id} \
                    from {remote_address}:{source_port} to {local_address}:{destination_port}",
                    match transport {
                        IncomingTrafficTransportType::Tcp => "TCP".to_string(),
                        IncomingTrafficTransportType::Tls {
                            alpn_protocol,
                            server_name,
                        } => format!(
                            "TLS (ALPN={:?}, SNI={server_name:?})",
                            alpn_protocol.as_deref().map(String::from_utf8_lossy),
                        ),
                    }
                );
            }
            message @ DaemonTcp::SubscribeResult(..) => {
                return Err(DumpSessionError::UnexpectedAgentMessage(
                    DaemonMessage::Tcp(message),
                ));
            }
        }

        Ok(())
    }

    async fn run(mut self, progress: &mut ProgressTracker) -> Result<Infallible, DumpSessionError> {
        self.init_connection().await?;

        loop {
            let message = tokio::select! {
                _ = self.ping_interval.tick() => {
                    tracing::debug!("Ping timeout reached, sending ping");
                    self.connection.sender.send(ClientMessage::Ping).await?;
                    continue;
                },

                message = self.connection.receiver.recv() => {
                    tracing::debug!(?message, "Received message");
                    message.ok_or(DumpSessionError::AgentConnClosed(None))?
                },
            };

            match message {
                DaemonMessage::Tcp(message) => {
                    self.handle_tcp_message(message, progress)?;
                }
                DaemonMessage::Close(message) => {
                    return Err(DumpSessionError::AgentConnClosed(Some(message)));
                }
                DaemonMessage::Pong => continue,
                DaemonMessage::LogMessage(LogMessage { level, message }) => match level {
                    LogLevel::Error => tracing::error!("Received log: {message}"),
                    LogLevel::Warn => tracing::warn!("Received log: {message}"),
                    LogLevel::Info => tracing::warn!("Received log: {message}"),
                },
                message @ (DaemonMessage::File(..)
                | DaemonMessage::GetAddrInfoResponse(..)
                | DaemonMessage::GetEnvVarsResponse(..)
                | DaemonMessage::PauseTarget(..)
                | DaemonMessage::SwitchProtocolVersionResponse(..)
                | DaemonMessage::TcpOutgoing(..)
                | DaemonMessage::UdpOutgoing(..)
                | DaemonMessage::Vpn(..)
                | DaemonMessage::TcpSteal(..)) => {
                    return Err(DumpSessionError::UnexpectedAgentMessage(message))
                }
            }
        }
    }
}

/// Provides a nice display of a request head (method, uri, version, headers).
struct RequestHead<'a, B>(&'a InternalHttpRequest<B>);

impl<B> fmt::Display for RequestHead<'_, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} {} {:?}", self.0.method, self.0.uri, self.0.version)?;
        for (header_name, header_value) in &self.0.headers {
            let value = String::from_utf8_lossy(header_value.as_bytes());
            writeln!(f, "{header_name}: {value}")?;
        }
        Ok(())
    }
}

/// Provides a nice display of a request frame.
struct RequestFrame {
    connection_id: ConnectionId,
    request_id: RequestId,
    frame: InternalHttpBodyFrame,
}

impl fmt::Display for RequestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "## Request ID [{}:{}] frame: ",
            self.connection_id, self.request_id
        )?;

        match &self.frame {
            InternalHttpBodyFrame::Data(data) => {
                writeln!(f, "data ({} bytes)", data.len())?;
                match std::str::from_utf8(data) {
                    Ok(s) => writeln!(f, "{s}")?,
                    Err(..) => writeln!(f, "{}\n(hex)", hex::encode(data.as_ref()))?,
                }
            }
            InternalHttpBodyFrame::Trailers(trailers) => {
                writeln!(f, "trailers")?;
                for (header_name, header_value) in trailers {
                    writeln!(
                        f,
                        "{header_name}: {}",
                        String::from_utf8_lossy(header_value.as_bytes())
                    )?;
                }
            }
        }

        Ok(())
    }
}

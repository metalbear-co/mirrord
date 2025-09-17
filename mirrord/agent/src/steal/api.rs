use std::{
    collections::{HashMap, VecDeque},
    error::Report,
    fmt,
    ops::{Not, RangeInclusive},
    sync::Arc,
    vec,
};

use bytes::Bytes;
use futures::{StreamExt, stream::FuturesUnordered};
use http_body_util::{BodyExt, combinators::BoxBody};
use hyper::Response;
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogMessage, Payload, RequestId,
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, ChunkedResponse, DaemonTcp,
        HTTP_CHUNKED_REQUEST_V2_VERSION, HTTP_CHUNKED_REQUEST_VERSION, HTTP_FRAMED_VERSION,
        HttpRequest, HttpRequestMetadata, HttpResponse, IncomingTrafficTransportType,
        InternalHttpBody, InternalHttpBodyFrame, InternalHttpBodyNew, InternalHttpRequest,
        LayerTcpSteal, MODE_AGNOSTIC_HTTP_REQUESTS, NewTcpConnectionV1, NewTcpConnectionV2,
        StealType, TcpClose, TcpData,
    },
};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_stream::StreamMap;
use tracing::Level;

use super::{Command, StealerCommand, StealerMessage};
use crate::{
    AgentError,
    error::AgentResult,
    http::{MIRRORD_AGENT_HTTP_HEADER_NAME, filter::HttpFilter},
    incoming::{
        ConnError, IncomingStream, IncomingStreamItem, RedirectorTaskConfig, ResponseBodyProvider,
        ResponseProvider, StolenHttp, StolenTcp,
    },
    steal::api::wait_body::WaitForFullBody,
    util::{ClientId, protocol_version::ClientProtocolVersion, remote_runtime::BgTaskStatus},
};

mod wait_body;

/// Bridges the communication between the agent and the [`TcpStealerTask`](super::TcpStealerTask).
///
/// There is an API instance for each connected agent client.
/// All API instances send commands to the task through a single shared channel.
/// However, each API instance has its own channel for receiving messages from the task.
///
/// # Note
///
/// In order to enable deprecation of old [`mirrord_protocol`] message variants,
/// this API should **always** produce the latest variants that can be handled by the client.
pub struct TcpStealerApi {
    /// Identifies the client owning of this API.
    client_id: ClientId,
    /// [`mirrord_protocol`] version of the client.
    protocol_version: ClientProtocolVersion,
    /// Channel that allows this API to communicate with the stealer task.
    command_tx: Sender<StealerCommand>,
    /// Channel that receives [`StealerMessage`]s from the stealer task.
    message_rx: Receiver<StealerMessage>,
    /// View on the task status.
    task_status: BgTaskStatus,
    /// Set of our active connections.
    connections: HashMap<ConnectionId, ClientConnectionState>,
    /// Streams of updates from our active connections.
    incoming_streams: StreamMap<ConnectionId, IncomingStream>,
    /// Requests that are in progress, waiting for their full body to be received.
    ///
    /// This provides compatibility with old [`mirrord_protocol`] versions
    /// that do not support chunked requests.
    requests_in_progress: FuturesUnordered<WaitForFullBody>,
    /// For assigning ids to new connections.
    connection_ids_iter: RangeInclusive<ConnectionId>,
    /// [`Self::recv`] and [`Self::handle_client_message`] can result in more than one message.
    ///
    /// We use this queue to store them and return from [`Self::recv`] one by one.
    queued_messages: VecDeque<DaemonMessage>,
}

impl TcpStealerApi {
    /// Size of the [`mpsc`] channel connecting this API
    /// with the background task.
    const CHANNEL_SIZE: usize = 64;

    /// Constant [`RequestId`] for stolen HTTP requests returned from this struct.
    ///
    /// Each stolen HTTP request is sent to the client with a distinct [`ConnectionId`]
    /// and [`RequestId`] 0. This makes the code simpler.
    ///
    /// Since `mirrord-intproxy` processes requests independently, this is fine.
    const REQUEST_ID: RequestId = 0;

    /// Creates a new [`TcpStealerApi`] instance.
    ///
    /// Given `command_tx` will be used to communicate with the
    /// [`TcpStealerTask`](super::TcpStealerTask).
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    pub(crate) async fn new(
        client_id: ClientId,
        protocol_version: ClientProtocolVersion,
        command_tx: Sender<StealerCommand>,
        task_status: BgTaskStatus,
    ) -> AgentResult<Self> {
        let (message_tx, message_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        let init_result = command_tx
            .send(StealerCommand {
                client_id,
                command: Command::NewClient(message_tx, protocol_version.clone()),
            })
            .await;
        if init_result.is_err() {
            return Err(task_status.wait_assert_running().await);
        }

        Ok(Self {
            client_id,
            protocol_version,
            command_tx,
            message_rx,
            task_status,
            connections: Default::default(),
            incoming_streams: Default::default(),
            requests_in_progress: Default::default(),
            connection_ids_iter: 0..=ConnectionId::MAX,
            queued_messages: Default::default(),
        })
    }

    /// Sends the `command` to the stealer task.
    async fn send_command(&mut self, command: Command) -> AgentResult<()> {
        let command = StealerCommand {
            client_id: self.client_id,
            command,
        };

        if self.command_tx.send(command).await.is_err() {
            Err(self.task_status.wait_assert_running().await)
        } else {
            Ok(())
        }
    }

    /// Returns a [`DaemonMessage`] to be sent to the client.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    pub(crate) async fn recv(&mut self) -> AgentResult<DaemonMessage> {
        loop {
            if let Some(message) = self.queued_messages.pop_front() {
                return Ok(message);
            }

            tokio::select! {
                message = self.message_rx.recv() => {
                    let Some(message) = message else {
                        // TcpStealerTask never removes clients on its own.
                        // It must have errored out or panicked.
                        return Err(self.task_status.wait_assert_running().await);
                    };

                    match message {
                        StealerMessage::Log(log) => {
                            break Ok(DaemonMessage::LogMessage(log));
                        },
                        StealerMessage::PortSubscribed(port) => {
                            break Ok(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(port))));
                        },
                        StealerMessage::StolenHttp(http) => self.handle_request(http)?,
                        StealerMessage::StolenTcp(tcp) => self.handle_connection(tcp)?,
                    }
                }

                Some(request) = self.requests_in_progress.next() => {
                    self.handle_request_completed(request)?;
                }

                Some((connection_id, item)) = self.incoming_streams.next() => {
                    self.handle_incoming_item(connection_id, item);
                }
            }
        }
    }

    /// Handles a stolen HTTP request received from the stealer task.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    fn handle_request(&mut self, request: StolenHttp) -> AgentResult<()> {
        let need_to_wait = self
            .protocol_version
            .matches(&HTTP_CHUNKED_REQUEST_VERSION)
            .not()
            && request.request_head.body_finished.not();
        if need_to_wait {
            self.requests_in_progress.push(request.into());
            return Ok(());
        }

        let connection_id = self
            .connection_ids_iter
            .next()
            .ok_or(AgentError::ExhaustedConnectionId)?;
        let StolenHttp {
            info,
            request_head,
            stream,
            response_provider,
            redirector_config,
        } = request;

        if self
            .protocol_version
            .matches(&HTTP_CHUNKED_REQUEST_V2_VERSION)
        {
            let message = DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(
                ChunkedRequest::StartV2(ChunkedRequestStartV2 {
                    connection_id,
                    request_id: Self::REQUEST_ID,
                    request: InternalHttpRequest {
                        method: request_head.method,
                        uri: request_head.uri,
                        headers: request_head.headers,
                        version: request_head.version,
                        body: InternalHttpBodyNew {
                            frames: request_head.body_head,
                            is_last: request_head.body_finished,
                        },
                    },
                    metadata: HttpRequestMetadata::V1 {
                        source: info.peer_addr,
                        destination: info.original_destination,
                    },
                    transport: info
                        .tls_connector
                        .map(|tls| IncomingTrafficTransportType::Tls {
                            server_name: tls.server_name().map(|s| s.to_str().into_owned()),
                            alpn_protocol: tls.alpn_protocol().map(Vec::from),
                        })
                        .unwrap_or(IncomingTrafficTransportType::Tcp),
                }),
            ));
            self.queued_messages.push_back(message);
        } else if self.protocol_version.matches(&HTTP_CHUNKED_REQUEST_VERSION) {
            let message = DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(
                ChunkedRequest::StartV1(HttpRequest {
                    internal_request: InternalHttpRequest {
                        method: request_head.method,
                        uri: request_head.uri,
                        headers: request_head.headers,
                        version: request_head.version,
                        body: request_head.body_head,
                    },
                    connection_id,
                    request_id: Self::REQUEST_ID,
                    port: info.original_destination.port(),
                }),
            ));
            self.queued_messages.push_back(message);

            if request_head.body_finished {
                let message = DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(
                    ChunkedRequest::Body(ChunkedRequestBodyV1 {
                        frames: Default::default(),
                        is_last: true,
                        connection_id,
                        request_id: Self::REQUEST_ID,
                    }),
                ));
                self.queued_messages.push_back(message);
            }
        } else if self.protocol_version.matches(&HTTP_FRAMED_VERSION) {
            let message = DaemonMessage::TcpSteal(DaemonTcp::HttpRequestFramed(HttpRequest {
                internal_request: InternalHttpRequest {
                    method: request_head.method,
                    uri: request_head.uri,
                    headers: request_head.headers,
                    version: request_head.version,
                    body: InternalHttpBody(request_head.body_head.into()),
                },
                connection_id,
                request_id: Self::REQUEST_ID,
                port: info.original_destination.port(),
            }));
            self.queued_messages.push_back(message);
        } else {
            let message = DaemonMessage::TcpSteal(DaemonTcp::HttpRequest(HttpRequest {
                internal_request: InternalHttpRequest {
                    method: request_head.method,
                    uri: request_head.uri,
                    headers: request_head.headers,
                    version: request_head.version,
                    body: frames_to_legacy(request_head.body_head),
                },
                connection_id,
                request_id: Self::REQUEST_ID,
                port: info.original_destination.port(),
            }));
            self.queued_messages.push_back(message);
        }

        self.incoming_streams.insert(connection_id, stream);
        self.connections.insert(
            connection_id,
            ClientConnectionState::HttpRequestSent {
                response_provider,
                redirector_config,
            },
        );

        Ok(())
    }

    /// Handles a stolen TCP connection received from the stealer task.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    fn handle_connection(&mut self, connection: StolenTcp) -> AgentResult<()> {
        let connection_id = self
            .connection_ids_iter
            .next()
            .ok_or(AgentError::ExhaustedConnectionId)?;
        let StolenTcp {
            info,
            stream,
            data_tx,
        } = connection;

        self.connections
            .insert(connection_id, ClientConnectionState::Tcp { data_tx });
        self.incoming_streams.insert(connection_id, stream);

        let new_connection = NewTcpConnectionV1 {
            connection_id,
            remote_address: info.peer_addr.ip(),
            destination_port: info.original_destination.port(),
            source_port: info.peer_addr.port(),
            local_address: info.local_addr.ip(),
        };

        let message = if self.protocol_version.matches(&MODE_AGNOSTIC_HTTP_REQUESTS) {
            DaemonTcp::NewConnectionV2(NewTcpConnectionV2 {
                connection: new_connection,
                transport: info
                    .tls_connector
                    .map(|tls| IncomingTrafficTransportType::Tls {
                        server_name: tls.server_name().map(|s| s.to_str().into_owned()),
                        alpn_protocol: tls.alpn_protocol().map(Vec::from),
                    })
                    .unwrap_or(IncomingTrafficTransportType::Tcp),
            })
        } else {
            DaemonTcp::NewConnectionV1(new_connection)
        };

        self.queued_messages
            .push_back(DaemonMessage::TcpSteal(message));

        Ok(())
    }

    /// Handles an incoming item from one of connections' streams.
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn handle_incoming_item(&mut self, connection_id: ConnectionId, item: IncomingStreamItem) {
        match item {
            IncomingStreamItem::Frame(frame) => {
                self.queued_messages.push_back(DaemonMessage::TcpSteal(
                    DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(ChunkedRequestBodyV1 {
                        frames: vec![frame],
                        is_last: false,
                        connection_id,
                        request_id: Self::REQUEST_ID,
                    })),
                ));
            }

            IncomingStreamItem::NoMoreFrames => {
                self.queued_messages.push_back(DaemonMessage::TcpSteal(
                    DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(ChunkedRequestBodyV1 {
                        frames: Default::default(),
                        is_last: true,
                        connection_id,
                        request_id: Self::REQUEST_ID,
                    })),
                ));
            }

            IncomingStreamItem::Data(bytes) => {
                self.queued_messages
                    .push_back(DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                        connection_id,
                        bytes: bytes.into(),
                    })));
            }

            IncomingStreamItem::NoMoreData => {
                self.queued_messages
                    .push_back(DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                        connection_id,
                        bytes: Default::default(),
                    })))
            }

            IncomingStreamItem::Finished(result) => {
                self.incoming_streams.remove(&connection_id);
                self.connections.remove(&connection_id);

                if let Err(error) = result {
                    self.queued_messages
                        .push_back(DaemonMessage::LogMessage(LogMessage::warn(format!(
                            "Stolen connection {connection_id} failed: {}",
                            Report::new(error),
                        ))));
                }

                self.queued_messages
                    .push_back(DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose {
                        connection_id,
                    })));
            }
        }
    }

    /// Handles a completed (full body read) HTTP request.
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn handle_request_completed(
        &mut self,
        request: Result<StolenHttp, ConnError>,
    ) -> AgentResult<()> {
        match request {
            Ok(request) => {
                let connection_id = self
                    .connection_ids_iter
                    .next()
                    .ok_or(AgentError::ExhaustedConnectionId)?;
                self.incoming_streams.insert(connection_id, request.stream);
                self.connections.insert(
                    connection_id,
                    ClientConnectionState::HttpRequestSent {
                        response_provider: request.response_provider,
                        redirector_config: request.redirector_config,
                    },
                );
                let message = if self.protocol_version.matches(&HTTP_FRAMED_VERSION) {
                    DaemonTcp::HttpRequestFramed(HttpRequest {
                        internal_request: InternalHttpRequest {
                            method: request.request_head.method,
                            uri: request.request_head.uri,
                            headers: request.request_head.headers,
                            version: request.request_head.version,
                            body: InternalHttpBody(
                                request.request_head.body_head.into_iter().collect(),
                            ),
                        },
                        connection_id,
                        request_id: Self::REQUEST_ID,
                        port: request.info.original_destination.port(),
                    })
                } else {
                    DaemonTcp::HttpRequest(HttpRequest {
                        internal_request: InternalHttpRequest {
                            method: request.request_head.method,
                            uri: request.request_head.uri,
                            headers: request.request_head.headers,
                            version: request.request_head.version,
                            body: frames_to_legacy(request.request_head.body_head),
                        },
                        connection_id,
                        request_id: Self::REQUEST_ID,
                        port: request.info.original_destination.port(),
                    })
                };
                self.queued_messages
                    .push_back(DaemonMessage::TcpSteal(message));
            }
            Err(error) => {
                self.queued_messages
                    .push_back(DaemonMessage::LogMessage(LogMessage::warn(format!(
                        "Stolen request failed: {}",
                        Report::new(error),
                    ))));
            }
        }

        Ok(())
    }

    /// Handles a [`LayerTcpSteal`] message from the client.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    pub(crate) async fn handle_client_message(
        &mut self,
        message: LayerTcpSteal,
    ) -> AgentResult<()> {
        match message {
            LayerTcpSteal::PortSubscribe(steal_type) => {
                let (port, filter) = match steal_type {
                    StealType::All(port) => (port, None),
                    StealType::FilteredHttp(port, filter) => (
                        port,
                        Some(
                            HttpFilter::try_from(&mirrord_protocol::tcp::HttpFilter::Header(
                                filter,
                            ))
                            .map_err(AgentError::InvalidHttpFilter)?,
                        ),
                    ),
                    StealType::FilteredHttpEx(port, filter) => (
                        port,
                        Some(HttpFilter::try_from(&filter).map_err(AgentError::InvalidHttpFilter)?),
                    ),
                };

                self.send_command(Command::PortSubscribe(port, filter))
                    .await?;
            }

            LayerTcpSteal::PortUnsubscribe(port) => {
                self.send_command(Command::PortUnsubscribe(port)).await?;
            }

            LayerTcpSteal::ConnectionUnsubscribe(connection_id) => {
                self.connections.remove(&connection_id);
                self.incoming_streams.remove(&connection_id);
            }

            LayerTcpSteal::Data(data) => {
                let Some(connection) = self.connections.get_mut(&data.connection_id) else {
                    return Ok(());
                };
                connection.send_data(data.bytes.0).await;
            }

            LayerTcpSteal::HttpResponse(response) => {
                if response.request_id != Self::REQUEST_ID {
                    return Ok(());
                }
                let Some(connection) = self.connections.get_mut(&response.connection_id) else {
                    return Ok(());
                };
                let response =
                    response.map_body(|body| std::iter::once(InternalHttpBodyFrame::Data(body)));
                connection.send_response(response, true).await;
            }

            LayerTcpSteal::HttpResponseFramed(response) => {
                if response.request_id != Self::REQUEST_ID {
                    return Ok(());
                }
                let Some(connection) = self.connections.get_mut(&response.connection_id) else {
                    return Ok(());
                };
                let response = response.map_body(|body| body.0);
                connection.send_response(response, true).await;
            }

            LayerTcpSteal::HttpResponseChunked(response) => match response {
                ChunkedResponse::Start(response) => {
                    if response.request_id != Self::REQUEST_ID {
                        return Ok(());
                    }
                    let Some(connection) = self.connections.get_mut(&response.connection_id) else {
                        return Ok(());
                    };
                    connection.send_response(response, false).await;
                }
                ChunkedResponse::Body(body) => {
                    if body.request_id != 0 {
                        return Ok(());
                    }
                    let Some(connection) = self.connections.get_mut(&body.connection_id) else {
                        return Ok(());
                    };
                    for frame in body.frames {
                        connection.send_response_frame(Some(frame)).await;
                    }
                    if body.is_last {
                        connection.send_response_frame(None).await;
                    }
                }
                ChunkedResponse::Error(error) => {
                    if error.request_id != Self::REQUEST_ID {
                        return Ok(());
                    }
                    self.incoming_streams.remove(&error.connection_id);
                    self.connections.remove(&error.connection_id);
                }
            },
        }

        Ok(())
    }
}

impl fmt::Debug for TcpStealerApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStealerApi")
            .field("client_id", &self.client_id)
            .finish()
    }
}

/// State of a stolen connection, from the perspective of the client.
enum ClientConnectionState {
    /// TCP connection, client is sending data.
    Tcp { data_tx: mpsc::Sender<Bytes> },
    /// HTTP request sent, waiting for the response from the client.
    HttpRequestSent {
        response_provider: ResponseProvider,
        redirector_config: Arc<RedirectorTaskConfig>,
    },
    /// HTTP request sent, response received, client is sending response body frames.
    HttpResponseReceived { body_provider: ResponseBodyProvider },
    /// HTTP request finished, connection upgraded, client is sending data.
    HttpUpgraded { data_tx: mpsc::Sender<Bytes> },
    /// TCP connection or HTTP request, client is no longer sending data.
    Closed,
}

impl ClientConnectionState {
    async fn send_data(&mut self, data: Bytes) {
        let sender = match self {
            Self::Tcp { data_tx } => data_tx,
            Self::HttpUpgraded { data_tx } => data_tx,
            _ => return,
        };

        if data.is_empty() {
            *self = Self::Closed;
            return;
        }

        let _ = sender.send(data).await;
    }

    async fn send_response_frame(&mut self, frame: Option<InternalHttpBodyFrame>) {
        let state = std::mem::replace(self, Self::Closed);
        let body_provider = match state {
            Self::HttpResponseReceived { body_provider } => body_provider,
            state => {
                *self = state;
                return;
            }
        };

        match frame {
            Some(frame) => {
                body_provider.send_frame(frame.into()).await;
                *self = Self::HttpResponseReceived { body_provider }
            }
            None => match body_provider.finish() {
                Some(data_tx) => *self = Self::HttpUpgraded { data_tx },
                None => {
                    *self = Self::Closed;
                }
            },
        }
    }

    async fn send_response<B>(&mut self, response: HttpResponse<B>, body_finished: bool)
    where
        B: IntoIterator<Item = InternalHttpBodyFrame>,
    {
        let state = std::mem::replace(self, Self::Closed);
        let (response_provider, redirector_config) = match state {
            Self::HttpRequestSent {
                response_provider,
                redirector_config,
            } => (response_provider, redirector_config),
            state => {
                *self = state;
                return;
            }
        };

        let parts = {
            let mut hyper_response = Response::new(());

            *hyper_response.status_mut() = response.internal_response.status;
            *hyper_response.headers_mut() = response.internal_response.headers;
            *hyper_response.version_mut() = response.internal_response.version;

            self.modify_response(&mut hyper_response, &redirector_config);

            hyper_response.into_parts().0
        };

        if body_finished {
            let body = InternalHttpBody(response.internal_response.body.into_iter().collect())
                .map_err(|_| unreachable!());
            let body = BoxBody::new(body);
            let response = Response::from_parts(parts, body);
            match response_provider.send_finished(response) {
                Some(data_tx) => *self = Self::HttpUpgraded { data_tx },
                None => {
                    *self = Self::Closed;
                }
            }
        } else {
            let body_provider = response_provider.send(parts);
            for frame in response.internal_response.body {
                body_provider.send_frame(frame.into()).await;
            }
            *self = Self::HttpResponseReceived { body_provider };
        }
    }

    /// Used for applying transformations on the response returned
    /// from the client. Currently just inserts the mirrord agent
    /// header.
    fn modify_response<T>(
        &self,
        response: &mut Response<T>,
        redirector_config: &RedirectorTaskConfig,
    ) {
        if redirector_config.inject_headers {
            response.headers_mut().insert(
                MIRRORD_AGENT_HTTP_HEADER_NAME,
                http::HeaderValue::from_static("forwarded-to-client"),
            );
        }
    }
}

/// Converts a vec of [`InternalHttpBodyFrame`]s to [`Payload`] (body format used in
/// [`DaemonTcp::HttpRequest`]).
fn frames_to_legacy(frames: Vec<InternalHttpBodyFrame>) -> Payload {
    frames
        .into_iter()
        .filter_map(|frame| match frame {
            InternalHttpBodyFrame::Data(data) => Some(data.0),
            InternalHttpBodyFrame::Trailers(..) => None,
        })
        .fold(Vec::new(), |mut add, data| {
            add.extend_from_slice(&data);
            add
        })
        .into()
}

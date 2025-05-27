use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    error::Report,
    fmt,
    ops::{Not, RangeInclusive},
    pin::Pin,
    task::{Context, Poll},
    vec,
};

use bytes::Bytes;
use futures::StreamExt;
use hyper::{
    body::{Body, Frame},
    Response,
};
use mirrord_protocol::{
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestErrorV1, ChunkedRequestStartV2,
        ChunkedResponse, DaemonTcp, HttpRequest, HttpRequestMetadata, HttpResponse,
        IncomingTrafficTransportType, InternalHttpBody, InternalHttpBodyFrame, InternalHttpBodyNew,
        InternalHttpRequest, LayerTcpSteal, NewTcpConnectionV1, NewTcpConnectionV2, StealType,
        TcpClose, TcpData, HTTP_CHUNKED_REQUEST_V2_VERSION, HTTP_CHUNKED_REQUEST_VERSION,
        HTTP_FRAMED_VERSION, MODE_AGNOSTIC_HTTP_REQUESTS,
    },
    ConnectionId, DaemonMessage, LogMessage, RequestId,
};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_stream::StreamMap;
use tracing::Level;

use super::{Command, StealerCommand, StealerMessage};
use crate::{
    error::AgentResult,
    http::filter::HttpFilter,
    incoming::{
        IncomingStream, IncomingStreamItem, ResponseBodyProvider, ResponseProvider, StolenHttp,
        StolenTcp,
    },
    util::{protocol_version::ClientProtocolVersion, remote_runtime::BgTaskStatus, ClientId},
    AgentError,
};

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
    connections: HashMap<ConnectionId, IncomingConnection>,
    /// Streams of updates from our active connections.
    incoming_streams: StreamMap<ConnectionId, IncomingStream>,
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
            connections: HashMap::new(),
            incoming_streams: StreamMap::new(),
            connection_ids_iter: 0..=ConnectionId::MAX,
            queued_messages: VecDeque::new(),
        })
    }

    /// Send the `command` to the stealer task,
    /// with the client id of the client that is using this API instance.
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

    async fn send_response(
        &mut self,
        response: HttpResponse<Vec<InternalHttpBodyFrame>>,
        body_finished: bool,
    ) {
        if response.request_id != Self::REQUEST_ID {
            return;
        }
        let Some(connection) = self.connections.get_mut(&response.connection_id) else {
            return;
        };
        let Some(response_tx) = connection.response_tx.take() else {
            return;
        };

        let mut hyper_response = Response::new(());
        *hyper_response.status_mut() = response.internal_response.status;
        *hyper_response.headers_mut() = response.internal_response.headers;
        *hyper_response.version_mut() = response.internal_response.version;
        let (parts, _) = hyper_response.into_parts();

        let body_sender = response_tx.send(parts);

        for frame in response.internal_response.body {
            body_sender.send_frame(frame.into()).await;
        }

        if body_finished {
            let data_tx = body_sender.finish();
            connection.data_tx = data_tx;
        } else {
            connection.response_body_tx.replace(body_sender);
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

                Some((connection_id, item)) = self.incoming_streams.next() => {
                    self.handle_incoming_item(connection_id, item);
                }
            }
        }
    }

    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    fn handle_request(&mut self, request: StolenHttp) -> AgentResult<()> {
        let connection_id = self
            .connection_ids_iter
            .next()
            .ok_or(AgentError::ExhaustedConnectionId)?;
        let StolenHttp {
            info,
            request_head,
            stream,
            response_provider,
        } = request;

        let mut connection =
            self.connections
                .entry(connection_id)
                .insert_entry(IncomingConnection {
                    data_tx: None,
                    request_in_progress: None,
                    response_tx: Some(response_provider),
                    response_body_tx: None,
                });
        self.incoming_streams.insert(connection_id, stream);

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
        } else if request_head.body_finished.not() {
            // Request body has not finished, and the client does not support chunked.
            // We need to wait for the body to finish.

            connection
                .get_mut()
                .request_in_progress
                .replace(HttpRequest {
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
                });
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

        Ok(())
    }

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

        self.connections.insert(
            connection_id,
            IncomingConnection {
                data_tx: Some(data_tx),
                request_in_progress: None,
                response_tx: None,
                response_body_tx: None,
            },
        );
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

    #[tracing::instrument(level = Level::TRACE, ret)]
    fn handle_incoming_item(&mut self, connection_id: ConnectionId, item: IncomingStreamItem) {
        match item {
            IncomingStreamItem::Frame(frame) => {
                self.handle_request_frame(connection_id, Some(frame));
            }

            IncomingStreamItem::NoMoreFrames => self.handle_request_frame(connection_id, None),

            IncomingStreamItem::Data(bytes) => {
                self.queued_messages
                    .push_back(DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                        connection_id,
                        bytes,
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

    #[tracing::instrument(level = Level::TRACE, ret)]
    fn handle_request_frame(
        &mut self,
        connection_id: ConnectionId,
        frame: Option<InternalHttpBodyFrame>,
    ) {
        let connection = self
            .connections
            .get_mut(&connection_id)
            .expect("connection not found, data structures in TcpStealerApi are out of sync");

        let Some(mut request) = connection.request_in_progress.take() else {
            let is_last = frame.is_none();
            self.queued_messages
                .push_back(DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(
                    ChunkedRequest::Body(ChunkedRequestBodyV1 {
                        frames: frame.into_iter().collect(),
                        is_last,
                        connection_id,
                        request_id: Self::REQUEST_ID,
                    }),
                )));

            return;
        };

        match frame {
            Some(frame) => {
                request.internal_request.body.0.push_back(frame);
                connection.request_in_progress.replace(request);
            }

            None if self.protocol_version.matches(&HTTP_FRAMED_VERSION) => {
                self.queued_messages.push_back(DaemonMessage::TcpSteal(
                    DaemonTcp::HttpRequestFramed(request),
                ));
            }

            None => {
                let request = request.map_body(|body| frames_to_legacy(body.0));
                self.queued_messages
                    .push_back(DaemonMessage::TcpSteal(DaemonTcp::HttpRequest(request)));
            }
        }
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
                let connection = self.connections.get_mut(&data.connection_id);

                let Some(connection) = connection else {
                    return Ok(());
                };

                if data.bytes.is_empty() {
                    connection.data_tx = None;
                } else if let Some(data_tx) = connection.data_tx.as_ref() {
                    let _ = data_tx.send(data.bytes).await;
                }
            }

            LayerTcpSteal::HttpResponse(response) => {
                let response = response.map_body(|body| vec![InternalHttpBodyFrame::Data(body)]);
                self.send_response(response, true).await;
            }

            LayerTcpSteal::HttpResponseFramed(response) => {
                let response = response.map_body(|body| body.0.into_iter().collect());
                self.send_response(response, true).await;
            }

            LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Start(response)) => {
                self.send_response(response, false).await;
            }

            LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(body)) => {
                let ChunkedRequestBodyV1 {
                    frames,
                    is_last,
                    request_id,
                    connection_id,
                } = body;

                if request_id != 0 {
                    return Ok(());
                }
                let Some(connection) = self.connections.get_mut(&connection_id) else {
                    return Ok(());
                };
                let Some(frame_tx) = connection.response_body_tx.take() else {
                    return Ok(());
                };

                for frame in frames {
                    frame_tx.send_frame(frame.into()).await;
                }

                if is_last {
                    let data_tx = frame_tx.finish();
                    connection.data_tx = data_tx;
                } else {
                    connection.response_body_tx.replace(frame_tx);
                }
            }

            LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Error(error)) => {
                let ChunkedRequestErrorV1 {
                    request_id,
                    connection_id,
                } = error;

                if request_id == 0 {
                    self.connections.remove(&connection_id);
                    self.incoming_streams.remove(&connection_id);
                }
            }
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

/// [`Body`] type we use when providing [`Response`]s to stolen requests.
struct ResponseBody {
    /// Frames from the initial HTTP response [`mirrord_protocol`] message.
    head: vec::IntoIter<InternalHttpBodyFrame>,
    /// Receiver for the rest of the frames.
    /// Closed when the client signalizes the end of the response body.
    rx: mpsc::Receiver<InternalHttpBodyFrame>,
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();

        if let Some(frame) = this.head.next() {
            return Poll::Ready(Some(Ok(frame.into())));
        }

        this.rx
            .poll_recv(cx)
            .map(|frame| frame.map(Frame::from).map(Ok))
    }
}

/// Represents a stolen connection in the [`TcpStealerApi`].
struct IncomingConnection {
    /// For sending data to the connection source.
    ///
    /// If this connection is an HTTP request, this is for sending data after the HTTP upgrade.
    data_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// If the client's [`mirrord_protocol`] version does not support
    /// [`DaemonTcp::HttpRequestChunked`], we must gather the incoming body frames here.
    ///
    /// When we've read the full bobdy, we send the full request in one of the legacy types.
    ///
    /// (only for HTTP requests)
    request_in_progress: Option<HttpRequest<InternalHttpBody>>,
    /// For sending the HTTP response.
    ///
    /// (only for HTTP requests)
    response_tx: Option<ResponseProvider>,
    /// For sending the HTTP response body frames.
    ///
    /// (only for HTTP requests)
    response_body_tx: Option<ResponseBodyProvider>,
}

fn frames_to_legacy<I: IntoIterator<Item = InternalHttpBodyFrame>>(frames: I) -> Vec<u8> {
    frames
        .into_iter()
        .filter_map(|frame| match frame {
            InternalHttpBodyFrame::Data(data) => Some(data),
            InternalHttpBodyFrame::Trailers(..) => None,
        })
        .reduce(|mut d1, d2| {
            d1.extend_from_slice(&d2);
            d1
        })
        .unwrap_or_default()
}

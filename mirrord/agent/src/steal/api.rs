use std::{
    collections::HashMap,
    convert::Infallible,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
    vec,
};

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::{Body, Frame},
    http::StatusCode,
    Response,
};
use mirrord_protocol::{
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, ChunkedResponse, DaemonTcp,
        HttpRequest, HttpRequestMetadata, HttpRequestTransportType, HttpResponse, InternalHttpBody,
        InternalHttpBodyFrame, InternalHttpBodyNew, InternalHttpRequest, LayerTcpSteal,
        NewTcpConnection, NewTcpConnectionV2, StealType, TcpClose, TcpData,
        HTTP_CHUNKED_REQUEST_V2_VERSION, HTTP_CHUNKED_REQUEST_VERSION, HTTP_FRAMED_VERSION,
    },
    ConnectionId, DaemonMessage, LogMessage, RequestId,
};
use tokio::sync::mpsc;
use tokio_stream::StreamMap;

use super::{Command, StealerCommand, StealerMessage};
use crate::{
    error::AgentResult,
    http::HttpFilter,
    incoming::{
        BoxBody, IncomingStream, IncomingStreamItem, ResponseProvider, StolenHttp, StolenTcp,
    },
    util::{
        protocol_version::SharedProtocolVersion,
        remote_runtime::BgTaskStatus,
        ClientId,
    },
};

mod request_state;

/// Bridges the communication between the agent and the [`TcpConnectionStealer`] task.
/// There is an API instance for each connected layer ("client"). All API instances send commands
/// On the same stealer command channel, where the layer-independent stealer listens to them.
pub struct TcpStealerApi {
    client_id: ClientId,
    protocol_version: SharedProtocolVersion,
    command_tx: mpsc::Sender<StealerCommand>,
    message_rx: mpsc::Receiver<StealerMessage>,
    task_status: BgTaskStatus,
    connections: HashMap<ConnectionId, IncomingConnection>,
    incoming_streams: StreamMap<ConnectionId, IncomingStream>,
    next_connection_id: ConnectionId,
}

impl TcpStealerApi {
    const REQUEST_ID: RequestId = 0;

    pub async fn new(
        client_id: ClientId,
        command_tx: mpsc::Sender<StealerCommand>,
        protocol_version: SharedProtocolVersion,
        task_status: BgTaskStatus,
        channel_size: usize,
    ) -> AgentResult<Self> {
        let (message_tx, message_rx) = mpsc::channel(channel_size);

        let init_result = command_tx
            .send(StealerCommand {
                client_id,
                command: Command::NewClient {
                    message_tx,
                    protocol_version: protocol_version.clone(),
                },
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
            next_connection_id: Default::default(),
        })
    }

    /// Send `command` to stealer, with the client id of the client that is using this API instance.
    async fn send_command(&mut self, command: Command) {
        let command = StealerCommand {
            client_id: self.client_id,
            command,
        };

        let _ = self.command_tx.send(command).await;
    }

    /// Helper function that passes the [`DaemonTcp`] messages we generated in the
    /// [`TcpConnectionStealer`] task, back to the agent.
    ///
    /// Called in the `ClientConnectionHandler`.
    pub async fn recv(&mut self) -> AgentResult<Vec<DaemonMessage>> {
        loop {
            let messages = tokio::select! {
                message = self.message_rx.recv() => {
                    let Some(message) = message else {
                        return Err(self.task_status.wait_assert_running().await);
                    };

                    match message {
                        StealerMessage::Log(log) => vec![DaemonMessage::LogMessage(log)],
                        StealerMessage::PortSubscribed(port) => vec![DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port)))],
                        StealerMessage::StolenHttp(http) => self.handle_request(http),
                        StealerMessage::StolenTcp(tcp) => vec![self.handle_connection(tcp)],
                    }
                }

                Some((connection_id, item)) = self.incoming_streams.next() => {
                    self.handle_incoming_item(connection_id, item)
                }
            };

            if messages.is_empty().not() {
                break Ok(messages);
            }
        }
    }

    fn next_connection_id(&mut self) -> ConnectionId {
        let connection_id = self.next_connection_id;
        self.next_connection_id += 1;
        connection_id
    }

    fn handle_request(&mut self, request: StolenHttp) -> Vec<DaemonMessage> {
        let connection_id = self.next_connection_id();
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
                    response_frame_tx: None,
                });
        self.incoming_streams.insert(connection_id, stream);

        if self
            .protocol_version
            .matches(&*HTTP_CHUNKED_REQUEST_V2_VERSION)
        {
            let message = DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(
                ChunkedRequest::StartV2(ChunkedRequestStartV2 {
                    connection_id,
                    request_id: Self::REQUEST_ID,
                    request: InternalHttpRequest {
                        method: request_head.method,
                        uri: request_head.uri,
                        headers: request_head.headers,
                        version: request_head.version,
                        body: InternalHttpBodyNew {
                            frames: request_head.body,
                            is_last: request_head.has_more_frames.not(),
                        },
                    },
                    metadata: HttpRequestMetadata::V1 {
                        source: info.peer_addr,
                        destination: info.original_destination,
                    },
                    transport: match info.tls_connector {
                        Some(tls) => HttpRequestTransportType::Tls {
                            alpn_protocol: tls.alpn_protocol().map(From::from),
                            server_name: tls.server_name().map(|name| name.to_str().into()),
                        },
                        None => HttpRequestTransportType::Tcp,
                    },
                }),
            ));
            vec![message]
        } else if request_head.has_more_frames.not() {
            // Request body has finished, so we can send just one message with framed/legacy.
            // Chunked v1 requires at least two messages to send the request.

            let framed = self.protocol_version.matches(&*HTTP_FRAMED_VERSION);

            let message = if framed {
                DaemonMessage::Tcp(DaemonTcp::HttpRequestFramed(HttpRequest {
                    internal_request: InternalHttpRequest {
                        method: request_head.method,
                        uri: request_head.uri,
                        headers: request_head.headers,
                        version: request_head.version,
                        body: InternalHttpBody(request_head.body.into()),
                    },
                    connection_id,
                    request_id: Self::REQUEST_ID,
                    port: info.original_destination.port(),
                }))
            } else {
                DaemonMessage::Tcp(DaemonTcp::HttpRequest(HttpRequest {
                    internal_request: InternalHttpRequest {
                        method: request_head.method,
                        uri: request_head.uri,
                        headers: request_head.headers,
                        version: request_head.version,
                        body: request_head
                            .body
                            .into_iter()
                            .filter_map(|frame| match frame {
                                InternalHttpBodyFrame::Data(data) => Some(data),
                                InternalHttpBodyFrame::Trailers(..) => None,
                            })
                            .reduce(|mut d1, d2| {
                                d1.extend_from_slice(&d2);
                                d1
                            })
                            .unwrap_or_default(),
                    },
                    connection_id,
                    request_id: Self::REQUEST_ID,
                    port: info.original_destination.port(),
                }))
            };
            vec![message]
        } else if self
            .protocol_version
            .matches(&*HTTP_CHUNKED_REQUEST_VERSION)
        {
            // The client supports chunked, so we can send the request head instantly.

            let mut messages = vec![DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(
                ChunkedRequest::StartV1(HttpRequest {
                    internal_request: InternalHttpRequest {
                        method: request_head.method,
                        uri: request_head.uri,
                        headers: request_head.headers,
                        version: request_head.version,
                        body: request_head.body,
                    },
                    connection_id,
                    request_id: Self::REQUEST_ID,
                    port: info.original_destination.port(),
                }),
            ))];

            if request_head.has_more_frames.not() {
                messages.push(DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(
                    ChunkedRequest::Body(ChunkedRequestBodyV1 {
                        frames: Default::default(),
                        is_last: true,
                        connection_id,
                        request_id: Self::REQUEST_ID,
                    }),
                )));
            }

            messages
        } else {
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
                        body: InternalHttpBody(request_head.body.into()),
                    },
                    connection_id,
                    request_id: Self::REQUEST_ID,
                    port: info.original_destination.port(),
                });

            Default::default()
        }
    }

    fn handle_connection(&mut self, connection: StolenTcp) -> DaemonMessage {
        let connection_id = self.next_connection_id();

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
                response_frame_tx: None,
            },
        );
        self.incoming_streams.insert(connection_id, stream);

        let new_connection = NewTcpConnection {
            connection_id,
            remote_address: info.peer_addr.ip(),
            destination_port: info.original_destination.port(),
            source_port: info.peer_addr.port(),
            local_address: info.local_addr.ip(),
        };

        let connection_message = match info.tls_connector {
            Some(connector) => DaemonTcp::NewConnectionV2(NewTcpConnectionV2 {
                connection: new_connection,
                transport: HttpRequestTransportType::Tls {
                    alpn_protocol: connector.alpn_protocol().map(From::from),
                    server_name: connector.server_name().map(|name| name.to_str().into()),
                },
            }),
            None => DaemonTcp::NewConnection(new_connection),
        };

        DaemonMessage::Tcp(connection_message)
    }

    fn handle_incoming_item(
        &mut self,
        connection_id: ConnectionId,
        item: IncomingStreamItem,
    ) -> Vec<DaemonMessage> {
        match item {
            IncomingStreamItem::Frame(frame) => self
                .handle_request_frame(connection_id, Some(frame))
                .into_iter()
                .map(DaemonMessage::Tcp)
                .collect(),

            IncomingStreamItem::NoMoreFrames => self
                .handle_request_frame(connection_id, None)
                .into_iter()
                .map(DaemonMessage::Tcp)
                .collect(),

            IncomingStreamItem::Data(bytes) => vec![DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id,
                bytes,
            }))],

            IncomingStreamItem::NoMoreData => vec![DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id,
                bytes: Default::default(),
            }))],

            IncomingStreamItem::Finished(result) => {
                self.incoming_streams.remove(&connection_id);
                self.connections.remove(&connection_id);

                let mut messages = vec![DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                    connection_id,
                }))];

                if let Err(error) = result {
                    messages.push(DaemonMessage::LogMessage(LogMessage::warn(format!(
                        "Stolen connection {connection_id} failed: {error}"
                    ))))
                }

                messages
            }
        }
    }

    fn handle_request_frame(
        &mut self,
        connection_id: ConnectionId,
        frame: Option<InternalHttpBodyFrame>,
    ) -> Option<DaemonTcp> {
        let connection = self.connections.get_mut(&connection_id)?;

        let Some(mut request) = connection.request_in_progress.take() else {
            let is_last = frame.is_none();
            let body = ChunkedRequestBodyV1 {
                frames: frame.into_iter().collect(),
                is_last,
                connection_id,
                request_id: Self::REQUEST_ID,
            };

            return Some(DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(body)));
        };

        match frame {
            Some(frame) => {
                request.internal_request.body.0.push_back(frame);
                connection.request_in_progress.replace(request);

                None
            }

            None if self.protocol_version.matches(&*HTTP_FRAMED_VERSION) => {
                Some(DaemonTcp::HttpRequestFramed(request))
            }

            None => {
                let request = request.map_body(|body| {
                    body.0
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
                });

                Some(DaemonTcp::HttpRequest(request))
            }
        }
    }

    fn send_response(
        &mut self,
        response: HttpResponse<BoxBody>,
        frame_tx: Option<mpsc::Sender<InternalHttpBodyFrame>>,
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

        let mut hyper_response = Response::new(response.internal_response.body);
        *hyper_response.status_mut() = response.internal_response.status;
        *hyper_response.headers_mut() = response.internal_response.headers;
        *hyper_response.version_mut() = response.internal_response.version;

        if hyper_response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let data_sender = response_tx.send_with_upgrade(hyper_response);
            connection.data_tx.replace(data_sender);
        } else {
            response_tx.send(hyper_response);
        }

        connection.response_frame_tx = frame_tx;
    }

    pub async fn handle_client_message(
        &mut self,
        message: LayerTcpSteal,
    ) -> Result<(), fancy_regex::Error> {
        match message {
            LayerTcpSteal::PortSubscribe(port_steal) => {
                let (port, filter) = match port_steal {
                    StealType::All(port) => (port, None),
                    StealType::FilteredHttp(port, filter) => {
                        let filter = HttpFilter::try_from(
                            &mirrord_protocol::tcp::HttpFilter::Header(filter),
                        )?;
                        (port, Some(filter))
                    }
                    StealType::FilteredHttpEx(port, filter) => {
                        let filter = HttpFilter::try_from(&filter)?;
                        (port, Some(filter))
                    }
                };

                self.send_command(Command::PortSubscribe { port, filter })
                    .await;
            }

            LayerTcpSteal::ConnectionUnsubscribe(connection_id) => {
                self.incoming_streams.remove(&connection_id);
                self.connections.remove(&connection_id);
            }

            LayerTcpSteal::PortUnsubscribe(port) => {
                self.send_command(Command::PortUnsubscribe(port)).await;
            }

            LayerTcpSteal::Data(tcp_data) => {
                let Some(connection) = self.connections.get_mut(&tcp_data.connection_id) else {
                    return Ok(());
                };

                if tcp_data.bytes.is_empty() {
                    connection.data_tx = None;
                    return Ok(());
                }

                let Some(data_tx) = &connection.data_tx else {
                    return Ok(());
                };

                let _ = data_tx.send(tcp_data.bytes).await;
            }

            LayerTcpSteal::HttpResponse(response) => {
                let response = response.map_body(|body| {
                    BoxBody::new(Full::new(Bytes::from_owner(body)).map_err(|_| unreachable!()))
                });
                self.send_response(response, None);
            }

            LayerTcpSteal::HttpResponseFramed(response) => {
                let response =
                    response.map_body(|body| BoxBody::new(body.map_err(|_| unreachable!())));
                self.send_response(response, None);
            }

            LayerTcpSteal::HttpResponseChunked(inner) => match inner {
                ChunkedResponse::Start(response) => {
                    let (frame_tx, frame_rx) = mpsc::channel(8);

                    let response = response.map_body(|body| {
                        let body = ResponseBody {
                            head: body.into_iter(),
                            rx: frame_rx,
                        };
                        let body = body.map_err(|_| unreachable!());
                        BoxBody::new(body)
                    });

                    self.send_response(response, Some(frame_tx));
                }

                ChunkedResponse::Body(body) => {
                    if body.request_id != 0 {
                        return Ok(());
                    }
                    let Some(connection) = self.connections.get_mut(&body.connection_id) else {
                        return Ok(());
                    };
                    let Some(frame_tx) = connection.response_frame_tx.as_mut() else {
                        return Ok(());
                    };

                    for frame in body.frames {
                        if frame_tx.send(frame).await.is_err() {
                            break;
                        }
                    }

                    if body.is_last {
                        connection.response_frame_tx = None;
                    }
                }

                ChunkedResponse::Error(error) => {
                    if error.request_id == 0 {
                        self.connections.remove(&error.connection_id);
                        self.incoming_streams.remove(&error.connection_id);
                    }
                }
            },
        }

        Ok(())
    }
}

struct ResponseBody {
    head: vec::IntoIter<InternalHttpBodyFrame>,
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

struct IncomingConnection {
    data_tx: Option<mpsc::Sender<Vec<u8>>>,
    request_in_progress: Option<HttpRequest<InternalHttpBody>>,
    response_tx: Option<ResponseProvider>,
    response_frame_tx: Option<mpsc::Sender<InternalHttpBodyFrame>>,
}

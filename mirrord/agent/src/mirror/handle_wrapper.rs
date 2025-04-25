use std::{
    collections::VecDeque,
    error::Report,
    ops::{Not, RangeInclusive},
};

use axum::async_trait;
use futures::StreamExt;
use mirrord_protocol::{
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, DaemonTcp,
        HttpRequestMetadata, InternalHttpBodyNew, InternalHttpRequest, LayerTcp, NewTcpConnection,
        NewTcpConnectionV2, TcpClose, TcpData, TrafficTransportType, NEW_CONNECTION_V2_VERSION,
    },
    ConnectionId, DaemonMessage, LogMessage, RequestId,
};
use tokio_stream::StreamMap;

use super::TcpMirrorApi;
use crate::{
    error::AgentResult,
    incoming::{
        IncomingStream, IncomingStreamItem, MirrorHandle, MirroredHttp, MirroredTcp,
        MirroredTraffic,
    },
    util::protocol_version::SharedProtocolVersion,
    AgentError,
};

/// Wrapper over [`MirrorHandle`], implementing [`TcpMirrorApi`].
pub struct MirrorHandleWrapper {
    handle: MirrorHandle,
    protocol_version: SharedProtocolVersion,
    /// Implementations of [`TcpMirrorApi`] methods can produce more that one message.
    ///
    /// We use this buffer to save them and return later from the next [`TcpMirrorApi::recv`] call.
    queued_messages: VecDeque<DaemonMessage>,
    incoming_streams: StreamMap<ConnectionId, IncomingStream>,
    connection_idx_iter: RangeInclusive<ConnectionId>,
}

impl MirrorHandleWrapper {
    /// Constant [`RequestId`] for mirrored HTTP requests returned from this struct.
    ///
    /// Each mirrored HTTP request is sent to the client with a distinct [`ConnectionId`]
    /// and constant [`RequestId`]. This makes the code simpler.
    ///
    /// Since `mirrord-intproxy` processes requests independently, it's fine.
    const REQUEST_ID: RequestId = 0;

    pub fn new(handle: MirrorHandle, protocol_version: SharedProtocolVersion) -> Self {
        Self {
            handle,
            protocol_version,
            queued_messages: Default::default(),
            incoming_streams: Default::default(),
            connection_idx_iter: (0..=ConnectionId::MAX),
        }
    }

    fn handle_tcp(&mut self, tcp: MirroredTcp) -> AgentResult<DaemonMessage> {
        let blocked = tcp.info.tls_connector.is_some()
            && self
                .protocol_version
                .matches(&NEW_CONNECTION_V2_VERSION)
                .not();
        if blocked {
            return Ok(DaemonMessage::LogMessage(LogMessage::warn(format!(
                "A TCP connection was not mirrored due to mirrord-protocol version requirement: {}",
                *NEW_CONNECTION_V2_VERSION,
            ))));
        }

        let id = self
            .connection_idx_iter
            .next()
            .ok_or(AgentError::ExhaustedConnectionId)?;
        self.incoming_streams.insert(id, tcp.stream);

        let new_connection = NewTcpConnection {
            connection_id: id,
            remote_address: tcp.info.peer_addr.ip(),
            local_address: tcp.info.local_addr.ip(),
            destination_port: tcp.info.original_destination.port(),
            source_port: tcp.info.peer_addr.port(),
        };

        let Some(tls) = tcp.info.tls_connector else {
            return Ok(DaemonMessage::Tcp(DaemonTcp::NewConnection(new_connection)));
        };

        Ok(DaemonMessage::Tcp(DaemonTcp::NewConnectionV2(
            NewTcpConnectionV2 {
                connection: new_connection,
                transport: TrafficTransportType::Tls {
                    alpn_protocol: tls.alpn_protocol().map(From::from),
                    server_name: tls.server_name().map(|name| name.to_str().into()),
                },
            },
        )))
    }

    fn handle_http(&mut self, http: MirroredHttp) -> AgentResult<DaemonMessage> {
        let blocked = self
            .protocol_version
            .matches(&NEW_CONNECTION_V2_VERSION)
            .not();
        if blocked {
            return Ok(DaemonMessage::LogMessage(LogMessage::warn(format!(
                "An HTTP request was not mirrored due to mirrord-protocol version requirement: {}",
                *NEW_CONNECTION_V2_VERSION,
            ))));
        }

        let id = self
            .connection_idx_iter
            .next()
            .ok_or(AgentError::ExhaustedConnectionId)?;
        self.incoming_streams.insert(id, http.stream);

        Ok(DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(
            ChunkedRequest::StartV2(ChunkedRequestStartV2 {
                connection_id: id,
                request_id: Self::REQUEST_ID,
                request: InternalHttpRequest {
                    method: http.request_head.method,
                    uri: http.request_head.uri,
                    headers: http.request_head.headers,
                    version: http.request_head.version,
                    body: InternalHttpBodyNew {
                        frames: http.request_head.body,
                        is_last: http.request_head.has_more_frames.not(),
                    },
                },
                metadata: HttpRequestMetadata::V1 {
                    source: http.info.peer_addr,
                    destination: http.info.original_destination,
                },
                transport: http
                    .info
                    .tls_connector
                    .map(From::from)
                    .unwrap_or(TrafficTransportType::Tcp),
            }),
        )))
    }

    fn handle_incoming_item(
        &mut self,
        connection_id: ConnectionId,
        item: IncomingStreamItem,
    ) -> DaemonMessage {
        match item {
            IncomingStreamItem::Data(bytes) => DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id,
                bytes,
            })),
            IncomingStreamItem::NoMoreData => DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id,
                bytes: Default::default(),
            })),
            IncomingStreamItem::Frame(frame) => DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(
                ChunkedRequest::Body(ChunkedRequestBodyV1 {
                    frames: vec![frame],
                    is_last: false,
                    connection_id,
                    request_id: Self::REQUEST_ID,
                }),
            )),
            IncomingStreamItem::NoMoreFrames => DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(
                ChunkedRequest::Body(ChunkedRequestBodyV1 {
                    frames: Default::default(),
                    is_last: true,
                    connection_id,
                    request_id: Self::REQUEST_ID,
                }),
            )),
            IncomingStreamItem::Finished(Ok(())) => {
                DaemonMessage::Tcp(DaemonTcp::Close(TcpClose { connection_id }))
            }
            IncomingStreamItem::Finished(Err(error)) => {
                self.queued_messages
                    .push_back(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                        connection_id,
                    })));
                DaemonMessage::LogMessage(LogMessage::warn(format!(
                    "Mirrored connection {connection_id} failed: {}",
                    Report::new(error),
                )))
            }
        }
    }
}

#[async_trait]
impl TcpMirrorApi for MirrorHandleWrapper {
    async fn recv(&mut self) -> Option<Result<DaemonMessage, AgentError>> {
        if let Some(message) = self.queued_messages.pop_front() {
            return Some(Ok(message));
        }

        let result = tokio::select! {
            Some(message) = self.handle.next() => match message {
                Err(error) => Err(error.into()),
                Ok(MirroredTraffic::Tcp(tcp)) => {
                    self.handle_tcp(tcp)
                },
                Ok(MirroredTraffic::Http(http)) => {
                    self.handle_http(http)
                },
            },

            Some((id, item)) = self.incoming_streams.next() => {
                Ok(self.handle_incoming_item(id, item))
            }

            else => return None,
        };

        Some(result)
    }

    async fn handle_client_message(&mut self, message: LayerTcp) -> Result<(), AgentError> {
        match message {
            LayerTcp::PortSubscribe(port) => {
                self.handle.mirror(port).await?;
                self.queued_messages
                    .push_back(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port))));
            }
            LayerTcp::PortUnsubscribe(port) => {
                self.handle.stop_mirror(port);
            }
            LayerTcp::ConnectionUnsubscribe(connection_id) => {
                self.incoming_streams.remove(&connection_id);
            }
        }

        Ok(())
    }
}

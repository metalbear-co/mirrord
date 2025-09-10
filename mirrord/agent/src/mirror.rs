use std::{
    collections::{HashMap, VecDeque},
    error::Report,
    ops::RangeInclusive,
};

use futures::StreamExt;
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogMessage, Port, RequestId,
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, DaemonTcp,
        HttpRequestMetadata, IncomingTrafficTransportType, InternalHttpBodyNew,
        InternalHttpRequest, LayerTcp, MODE_AGNOSTIC_HTTP_REQUESTS, NewTcpConnectionV1,
        NewTcpConnectionV2, TcpClose, TcpData,
    },
};
use tokio_stream::StreamMap;

use crate::{
    AgentError,
    error::AgentResult,
    http::filter::HttpFilter,
    incoming::{IncomingStream, IncomingStreamItem, MirrorHandle, MirroredTraffic},
    sniffer::api::TcpSnifferApi,
    util::protocol_version::ClientProtocolVersion,
};

/// Agent client's API for using the TCP mirror feature.
pub enum TcpMirrorApi {
    /// Wrapper over a [`MirrorHandle`].
    ///
    /// Mirroring is done with iptables redirections.
    Passthrough {
        mirror_handle: MirrorHandle,
        incoming_streams: StreamMap<ConnectionId, IncomingStream>,
        protocol_version: ClientProtocolVersion,
        connection_ids_iter: RangeInclusive<ConnectionId>,
        queued_messages: VecDeque<DaemonTcp>,
        port_filters: HashMap<Port, HttpFilter>,
    },
    /// Wrapper over a [`TcpSnifferApi`].
    ///
    /// Mirroring is done with a raw socket.
    Sniffer {
        api: TcpSnifferApi,
        queued_message: Option<DaemonTcp>,
    },
}

impl TcpMirrorApi {
    /// Constant [`RequestId`] for mirrored HTTP requests returned from this struct.
    ///
    /// Each mirrored HTTP request is sent to the client with a distinct [`ConnectionId`]
    /// and [`RequestId`] 0. This makes the code simpler.
    ///
    /// Since `mirrord-intproxy` processes requests independently, this is fine.
    const REQUEST_ID: RequestId = 0;

    pub fn passthrough(
        mirror_handle: MirrorHandle,
        protocol_version: ClientProtocolVersion,
    ) -> Self {
        Self::Passthrough {
            mirror_handle,
            incoming_streams: Default::default(),
            protocol_version,
            connection_ids_iter: 0..=ConnectionId::MAX,
            queued_messages: Default::default(),
            port_filters: Default::default(),
        }
    }

    pub fn sniffer(api: TcpSnifferApi) -> Self {
        Self::Sniffer {
            api,
            queued_message: Default::default(),
        }
    }

    pub async fn handle_client_message(&mut self, message: LayerTcp) -> AgentResult<()> {
        match self {
            Self::Passthrough {
                mirror_handle,
                incoming_streams,
                queued_messages,
                port_filters,
                ..
            } => match message {
                LayerTcp::ConnectionUnsubscribe(id) => {
                    incoming_streams.remove(&id);
                }
                LayerTcp::PortSubscribe(port) => {
                    mirror_handle.mirror(port).await?;
                    queued_messages.push_back(DaemonTcp::SubscribeResult(Ok(port)));
                }
                LayerTcp::PortSubscribeFilteredHttp(port, filter) => {
                    // Convert from protocol HttpFilter to agent HttpFilter
                    let agent_filter =
                        HttpFilter::try_from(&filter).map_err(AgentError::InvalidHttpFilter)?;
                    port_filters.insert(port, agent_filter);

                    mirror_handle.mirror(port).await?;
                    queued_messages.push_back(DaemonTcp::SubscribeResult(Ok(port)));
                }
                LayerTcp::PortUnsubscribe(port) => {
                    port_filters.remove(&port);
                    mirror_handle.stop_mirror(port);
                }
            },
            Self::Sniffer { api, .. } => {
                // Sniffer mode doesn't support HTTP filtering, so ignore filtered subscriptions
                if let LayerTcp::PortSubscribeFilteredHttp(_, _) = &message {
                    return Ok(());
                }
                api.handle_client_message(message).await?;
            }
        }

        Ok(())
    }

    pub async fn recv(&mut self) -> AgentResult<DaemonMessage> {
        let message = match self {
            Self::Passthrough {
                mirror_handle,
                incoming_streams,
                protocol_version,
                connection_ids_iter,
                queued_messages,
                port_filters,
            } => {
                if let Some(message) = queued_messages.pop_front() {
                    return Ok(DaemonMessage::Tcp(message));
                }

                tokio::select! {
                    Some((id, item)) = incoming_streams.next() => match item {
                        IncomingStreamItem::Data(data) => {
                            DaemonTcp::Data(TcpData {
                                connection_id: id,
                                bytes: data.into(),
                            })
                        }
                        IncomingStreamItem::NoMoreData => {
                            DaemonTcp::Data(TcpData {
                                connection_id: id,
                                bytes: Default::default(),
                            })
                        }
                        IncomingStreamItem::Frame(frame) => {
                            DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(ChunkedRequestBodyV1 {
                                frames: vec![frame],
                                is_last: false,
                                connection_id: id,
                                request_id: Self::REQUEST_ID,
                            }))
                        }
                        IncomingStreamItem::NoMoreFrames => {
                            DaemonTcp::HttpRequestChunked(ChunkedRequest::Body(ChunkedRequestBodyV1 {
                                frames: Default::default(),
                                is_last: true,
                                connection_id: id,
                                request_id: Self::REQUEST_ID,
                            }))
                        }
                        IncomingStreamItem::Finished(Ok(())) => {
                            DaemonTcp::Close(TcpClose {
                                connection_id: id,
                            })
                        }
                        IncomingStreamItem::Finished(Err(error)) => {
                            queued_messages.push_back(DaemonTcp::Close(TcpClose { connection_id: id }));
                            return Ok(DaemonMessage::LogMessage(LogMessage::warn(format!("Mirrored connection {id} failed: {}", Report::new(error)))));
                        }
                    },

                    Some(traffic) = mirror_handle.next() => match traffic? {
                        MirroredTraffic::Tcp(tcp) if protocol_version.matches(&MODE_AGNOSTIC_HTTP_REQUESTS) => {
                            if port_filters.contains_key(&tcp.info.original_destination.port()) {
                                return Ok(DaemonMessage::LogMessage(LogMessage::warn(
                                    "TCP traffic skipped due to HTTP filter on this port".to_string()
                                )));
                            }

                            let id = connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;
                            let connection = NewTcpConnectionV1 {
                                connection_id: id,
                                remote_address: tcp.info.peer_addr.ip(),
                                destination_port: tcp.info.original_destination.port(),
                                source_port: tcp.info.peer_addr.port(),
                                local_address: tcp.info.local_addr.ip(),
                            };
                            let message = NewTcpConnectionV2 {
                                connection,
                                transport: tcp
                                    .info
                                    .tls_connector
                                    .map(|tls| IncomingTrafficTransportType::Tls {
                                        alpn_protocol: tls.alpn_protocol().map(From::from),
                                        server_name: tls.server_name().map(|s| s.to_str().into_owned()),
                                    })
                                    .unwrap_or(IncomingTrafficTransportType::Tcp),
                            };
                            incoming_streams.insert(id, tcp.stream);
                            DaemonTcp::NewConnectionV2(message)
                        }

                        MirroredTraffic::Tcp(tcp) => {
                            if tcp.info.tls_connector.is_some() {
                                return Ok(DaemonMessage::LogMessage(LogMessage::error(format!(
                                    "A TLS connection was not mirrored due to mirrord-protocol version requirement: {}",
                                    &*MODE_AGNOSTIC_HTTP_REQUESTS,
                                ))));
                            }

                            if port_filters.contains_key(&tcp.info.original_destination.port()) {
                                return Ok(DaemonMessage::LogMessage(LogMessage::warn(
                                    "TCP traffic skipped due to HTTP filter on this port".to_string()
                                )));
                            }

                            let id = connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;
                            incoming_streams.insert(id, tcp.stream);

                            let message = NewTcpConnectionV1 {
                                connection_id: id,
                                remote_address: tcp.info.peer_addr.ip(),
                                destination_port: tcp.info.original_destination.port(),
                                source_port: tcp.info.peer_addr.port(),
                                local_address: tcp.info.local_addr.ip(),
                            };
                            DaemonTcp::NewConnectionV1(message)
                        }

                        MirroredTraffic::Http(mut http) if protocol_version.matches(&MODE_AGNOSTIC_HTTP_REQUESTS) => {
                            if let Some(filter) = port_filters.get(&http.info.original_destination.port()) && !filter.matches(http.parts_mut()) {
                                return Ok(DaemonMessage::LogMessage(LogMessage::warn(format!(
                                    "HTTP request filtered out: {} {}",
                                    http.parts().method,
                                    http.parts().uri
                                ))));
                            }

                            let id = connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;
                            incoming_streams.insert(id, http.stream);

                            let message = ChunkedRequestStartV2 {
                                connection_id: id,
                                request_id: Self::REQUEST_ID,
                                metadata: HttpRequestMetadata::V1 {
                                    source: http.info.peer_addr,
                                    destination: http.info.original_destination,
                                },
                                transport: http
                                    .info
                                    .tls_connector
                                    .map(|tls| IncomingTrafficTransportType::Tls {
                                        alpn_protocol: tls.alpn_protocol().map(From::from),
                                        server_name: tls.server_name().map(|s| s.to_str().into_owned()),
                                    })
                                    .unwrap_or(IncomingTrafficTransportType::Tcp),
                                request: InternalHttpRequest {
                                    method: http.request_head.method,
                                    uri: http.request_head.uri,
                                    headers: http.request_head.headers,
                                    version: http.request_head.version,
                                    body: InternalHttpBodyNew {
                                        frames: http.request_head.body_head,
                                        is_last: http.request_head.body_finished,
                                    },
                                },
                            };
                            DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV2(message))
                        }

                        MirroredTraffic::Http(..) => {
                            return Ok(DaemonMessage::LogMessage(LogMessage::error(format!(
                                "An HTTP request was not mirrored due to mirrord-protocol version requirement: {}",
                                &*MODE_AGNOSTIC_HTTP_REQUESTS,
                            ))));
                        }
                    },

                    else => std::future::pending().await,
                }
            }

            Self::Sniffer {
                api,
                queued_message,
            } => {
                if let Some(message) = queued_message.take() {
                    return Ok(DaemonMessage::Tcp(message));
                }

                let (message, log) = api.recv().await?;
                if let Some(log) = log {
                    queued_message.replace(message);
                    return Ok(DaemonMessage::LogMessage(log));
                }
                message
            }
        };

        Ok(DaemonMessage::Tcp(message))
    }
}

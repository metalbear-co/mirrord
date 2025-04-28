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
    /// Determines which subset of the stolen traffic we can receive.
    protocol_version: SharedProtocolVersion,
    /// Implementations of [`TcpMirrorApi`] methods can produce more that one message.
    ///
    /// We use this buffer to save them and return later from the next [`TcpMirrorApi::recv`] call.
    queued_messages: VecDeque<DaemonMessage>,
    /// Streams of updates from our active connections.
    incoming_streams: StreamMap<ConnectionId, IncomingStream>,
    /// For assigning ids to new connections.
    connection_ids_iter: RangeInclusive<ConnectionId>,
}

impl MirrorHandleWrapper {
    /// Constant [`RequestId`] for mirrored HTTP requests returned from this struct.
    ///
    /// Each mirrored HTTP request is sent to the client with a distinct [`ConnectionId`]
    /// and constant [`RequestId`]. This makes the code simpler.
    ///
    /// Since `mirrord-intproxy` processes requests independently, this is fine.
    const REQUEST_ID: RequestId = 0;

    pub fn new(handle: MirrorHandle, protocol_version: SharedProtocolVersion) -> Self {
        Self {
            handle,
            protocol_version,
            queued_messages: Default::default(),
            incoming_streams: Default::default(),
            connection_ids_iter: (0..=ConnectionId::MAX),
        }
    }

    fn handle_tcp(&mut self, tcp: MirroredTcp) -> AgentResult<DaemonMessage> {
        let blocked = tcp.info.tls_connector.is_some()
            && self
                .protocol_version
                .matches(&NEW_CONNECTION_V2_VERSION)
                .not();
        if blocked {
            return Ok(DaemonMessage::LogMessage(LogMessage::error(format!(
                "A TCP connection was not mirrored due to mirrord-protocol version requirement: {}",
                *NEW_CONNECTION_V2_VERSION,
            ))));
        }

        let id = self
            .connection_ids_iter
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
            return Ok(DaemonMessage::LogMessage(LogMessage::error(format!(
                "An HTTP request was not mirrored due to mirrord-protocol version requirement: {}",
                *NEW_CONNECTION_V2_VERSION,
            ))));
        }

        let id = self
            .connection_ids_iter
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

#[cfg(test)]
mod test {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        sync::Arc,
        time::Duration,
    };

    use bytes::Bytes;
    use http_body_util::{combinators::BoxBody, BodyExt, Empty};
    use hyper::Request;
    use mirrord_agent_env::steal_tls::{
        AgentClientConfig, AgentServerConfig, IncomingPortTlsConfig, TlsAuthentication,
        TlsServerVerification,
    };
    use mirrord_protocol::{
        tcp::{DaemonTcp, NewTcpConnectionV2, TrafficTransportType},
        DaemonMessage, LogLevel, LogMessage,
    };
    use pem::{EncodeConfig, LineEnding, Pem};
    use rstest::rstest;
    use rustls::{ClientConfig, RootCertStore};
    use tokio::{io::AsyncWriteExt, sync::Notify};
    use tokio_rustls::TlsConnector;

    use crate::{
        http::HttpVersion,
        incoming::{test::start_redirector_task, tls::IncomingTlsHandlerStore},
        mirror::TcpMirrorApi,
        test::get_mirror_api_with_subscription,
        util::path_resolver::InTargetPathResolver,
    };

    /// Verifies that the client receives an error log when an HTTP request is not mirrored due to
    /// [`mirrord-protocol`] version.
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn compat_test_http_not_supported() {
        let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

        let (mut connections, _, mirror_handle) = start_redirector_task(Default::default()).await;
        let (mut mirror, protocol) =
            get_mirror_api_with_subscription(mirror_handle, Some("1.19.3"), destination.port())
                .await;

        let notify = Arc::new(Notify::new());
        let notify_cloned = notify.clone();

        tokio::spawn(async move {
            let mut request_sender = connections.new_http(destination, HttpVersion::V1).await;
            let _ = request_sender
                .send(Request::new(BoxBody::new(
                    Empty::<Bytes>::new().map_err(|_| unreachable!()),
                )))
                .await
                .unwrap();
            notify_cloned.notified().await;
            let _ = request_sender
                .send(Request::new(BoxBody::new(
                    Empty::<Bytes>::new().map_err(|_| unreachable!()),
                )))
                .await
                .unwrap();
        });

        match mirror.recv().await.unwrap().unwrap() {
            DaemonMessage::LogMessage(LogMessage {
                level: LogLevel::Error,
                ..
            }) => {}
            other => panic!("unexpected message: {other:?}"),
        }

        protocol.replace("1.19.4".parse().unwrap());
        notify.notify_one();

        match mirror.recv().await.unwrap().unwrap() {
            DaemonMessage::Tcp(DaemonTcp::HttpRequestChunked(..)) => {}
            other => panic!("unexpected message: {other:?}"),
        }
    }

    /// Verifies that the client receives an error log when a TLS connection is not mirrored due to
    /// [`mirrord-protocol`] version.
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn compat_test_tls_not_supported() {
        let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

        let tempdir = tempfile::tempdir().unwrap();
        let root_cert = mirrord_tls_util::generate_cert("root", None, true).unwrap();
        let server_cert =
            mirrord_tls_util::generate_cert("server", Some(&root_cert), false).unwrap();
        let cert_file = tempdir.path().join("cert.pem");
        let cert_pem = pem::encode_many_config(
            &[
                Pem::new("PRIVATE KEY", server_cert.key_pair.serialize_der()),
                Pem::new("CERTIFICATE", server_cert.cert.der().to_vec()),
                Pem::new("CERTIFICATE", root_cert.cert.der().to_vec()),
            ],
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        );
        tokio::fs::write(cert_file, cert_pem).await.unwrap();
        let tls_config = IncomingPortTlsConfig {
            port: destination.port(),
            agent_as_server: AgentServerConfig {
                authentication: TlsAuthentication {
                    cert_pem: "cert.pem".into(),
                    key_pem: "cert.pem".into(),
                },
                verification: None,
                alpn_protocols: vec![],
            },
            agent_as_client: AgentClientConfig {
                authentication: None,
                verification: TlsServerVerification {
                    accept_any_cert: false,
                    trust_roots: vec!["cert.pem".into()],
                },
            },
        };
        let tls_store = IncomingTlsHandlerStore::new(
            vec![tls_config],
            InTargetPathResolver::with_root_path(tempdir.path().to_path_buf()),
        );

        let mut root_store = RootCertStore::empty();
        root_store.add(root_cert.cert.into()).unwrap();
        let connector = TlsConnector::from(Arc::new(
            ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        ));

        let (mut connections, _, mirror_handle) = start_redirector_task(tls_store).await;
        let (mut mirror, protocol) =
            get_mirror_api_with_subscription(mirror_handle, Some("1.19.3"), destination.port())
                .await;

        let notify = Arc::new(Notify::new());
        let notify_cloned = notify.clone();

        tokio::spawn(async move {
            let connection = connections.new_tcp(destination).await;
            let mut connection = connector
                .connect("server".try_into().unwrap(), connection)
                .await
                .unwrap();
            connection
                .write_all(b" i am not http trust me")
                .await
                .unwrap();
            notify_cloned.notified().await;
            let connection = connections.new_tcp(destination).await;
            let mut connection = connector
                .connect("server".try_into().unwrap(), connection)
                .await
                .unwrap();
            connection
                .write_all(b" i am not http trust me")
                .await
                .unwrap();
        });

        match mirror.recv().await.unwrap().unwrap() {
            DaemonMessage::LogMessage(LogMessage {
                level: LogLevel::Error,
                ..
            }) => {}
            other => panic!("unexpected message: {other:?}"),
        }

        protocol.replace("1.19.4".parse().unwrap());
        notify.notify_one();

        match mirror.recv().await.unwrap().unwrap() {
            DaemonMessage::Tcp(DaemonTcp::NewConnectionV2(NewTcpConnectionV2 {
                transport: TrafficTransportType::Tls { .. },
                ..
            })) => {}
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

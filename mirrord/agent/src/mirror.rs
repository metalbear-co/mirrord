use std::{
    collections::{HashMap, VecDeque},
    ops::RangeInclusive,
};

use futures::StreamExt;
use mirrord_nightly_polyfill::error::Report;
use mirrord_protocol::{
    ConnectionId, DaemonMessage, LogMessage, Port, RequestId,
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestStartV2, DaemonTcp,
        HttpRequestMetadata, IncomingTrafficTransportType, InternalHttpBodyNew,
        InternalHttpRequest, LayerTcp, MODE_AGNOSTIC_HTTP_REQUESTS, NewTcpConnectionV1,
        NewTcpConnectionV2, TcpClose, TcpData,
    },
};
use tokio::task::JoinSet;
use tokio_stream::StreamMap;
use tracing::{Level, instrument};

use crate::{
    AgentError,
    error::AgentResult,
    http::filter::{HttpFilter, MatchResult},
    incoming::{
        IncomingStream, IncomingStreamItem, MirrorHandle, MirroredHttp, MirroredTraffic,
        RedirectorTaskError,
    },
    util::protocol_version::ClientProtocolVersion,
};

/// Agent client's API for using the TCP mirror feature.
///
/// Wrapper over a [`MirrorHandle`].
pub struct TcpMirrorApi {
    mirror_handle: MirrorHandle,
    incoming_streams: StreamMap<ConnectionId, IncomingStream>,
    protocol_version: ClientProtocolVersion,
    connection_ids_iter: RangeInclusive<ConnectionId>,
    queued_messages: VecDeque<DaemonTcp>,
    port_filters: HashMap<Port, HttpFilter>,
    ongoing_requests: JoinSet<MirroredHttp>,
}

impl TcpMirrorApi {
    /// Constant [`RequestId`] for mirrored HTTP requests returned from this struct.
    ///
    /// Each mirrored HTTP request is sent to the client with a distinct [`ConnectionId`]
    /// and [`RequestId`] 0. This makes the code simpler.
    ///
    /// Since `mirrord-intproxy` processes requests independently, this is fine.
    const REQUEST_ID: RequestId = 0;

    pub fn new(mirror_handle: MirrorHandle, protocol_version: ClientProtocolVersion) -> Self {
        Self {
            mirror_handle,
            incoming_streams: Default::default(),
            protocol_version,
            connection_ids_iter: 0..=ConnectionId::MAX,
            queued_messages: Default::default(),
            port_filters: Default::default(),
            ongoing_requests: Default::default(),
        }
    }

    pub async fn handle_client_message(&mut self, message: LayerTcp) -> AgentResult<()> {
        match message {
            LayerTcp::ConnectionUnsubscribe(id) => {
                self.incoming_streams.remove(&id);
            }
            LayerTcp::PortSubscribe(port) => {
                self.mirror_handle.mirror(port).await?;
                self.queued_messages
                    .push_back(DaemonTcp::SubscribeResult(Ok(port)));
            }
            LayerTcp::PortSubscribeFilteredHttp(port, filter) => {
                // Convert from protocol HttpFilter to agent HttpFilter
                let agent_filter = HttpFilter::try_from(&filter)
                    .map_err(Box::new)
                    .map_err(AgentError::InvalidHttpFilter)?;

                self.mirror_handle.mirror(port).await?;
                self.port_filters.insert(port, agent_filter);
                self.queued_messages
                    .push_back(DaemonTcp::SubscribeResult(Ok(port)));
            }
            LayerTcp::PortUnsubscribe(port) => {
                self.port_filters.remove(&port);
                self.mirror_handle.stop_mirror(port);
            }
        }

        Ok(())
    }

    #[instrument(level = Level::TRACE, ret)]
    async fn next(
        handle: &mut MirrorHandle,
        ongoing: &mut JoinSet<MirroredHttp>,
        version: &ClientProtocolVersion,
        filters: &HashMap<Port, HttpFilter>,
    ) -> Result<MirroredTraffic, RedirectorTaskError> {
        use MirroredTraffic as M;
        loop {
            return tokio::select! {
                Some(finished) = ongoing.join_next() => {
                    match finished {
                        Ok(mut http) => {
                            let port = http.info.original_destination.port();
                            let Some(filter) = filters.get(&port) else {
                                tracing::warn!("have no filter for request with buffered body.");
                                return Ok(M::Http(http));
                            };

                            // Buffering either completed the body or gave up on it for good, so
                            // the filter gets to see every buffered request, including one whose
                            // body never finished: a `Not` over a body filter mirrors exactly
                            // those. Only the filter decides here; it never asks for the body
                            // again after a buffering attempt, and if it did, dropping the request
                            // beats buffering it twice.
                            let (parts, body) = http.parts_and_body();
                            match filter.matches(parts, body).await {
                                MatchResult::Match => Ok(M::Http(http)),
                                MatchResult::Undecided if filter.undecided_matches(body) => {
                                    Ok(M::Http(http))
                                }
                                MatchResult::NoMatch
                                | MatchResult::Undecided
                                | MatchResult::NeedsBody => continue,
                            }
                        },
                        Err(error) => {
                            tracing::error!(
                                ?error,
                                "HTTP mirror body buffer task panicked. This is a bug in the agent, please report it"
                            );
                            continue
                        }
                    }
                }
                Some(next) = handle.next() => {
                    match next? {
                        M::Tcp(tcp) => {
                            if filters.contains_key(&tcp.info.original_destination.port()) {
                                continue
                            } else {
                                Ok(M::Tcp(tcp))
                            }
                        }

                        M::Http(mut http) if version.matches(&MODE_AGNOSTIC_HTTP_REQUESTS) => {
                            let port = http.info.original_destination.port();
                            let Some(filter) = filters.get(&port) else {
                                return Ok(M::Http(http));
                            };

                            let (parts, body) = http.parts_and_body();
                            match filter.matches(parts, body).await {
                                MatchResult::Match => return Ok(M::Http(http)),
                                MatchResult::Undecided if filter.undecided_matches(body) => {
                                    return Ok(M::Http(http));
                                }
                                MatchResult::NoMatch | MatchResult::Undecided => continue,
                                MatchResult::NeedsBody => {
                                    ongoing.spawn(async move {
                                        if let Err(error) = http.buffer_body().await {
                                            tracing::debug!(?error, "failed to buffer request body");
                                        };
                                        http
                                    });
                                    continue
                                }
                            }
                        }

                        M::Http(http) => {
                            Ok(M::Http(http))
                        }
                    }
                }
                else => std::future::pending().await,
            };
        }
    }

    pub async fn recv(&mut self) -> AgentResult<DaemonMessage> {
        if let Some(message) = self.queued_messages.pop_front() {
            return Ok(DaemonMessage::Tcp(message));
        }

        let message = tokio::select! {
            Some((id, item)) = self.incoming_streams.next() => match item {
                IncomingStreamItem::Data(data) => DaemonTcp::Data(TcpData {
                    connection_id: id,
                    bytes: data.into(),
                }),
                IncomingStreamItem::NoMoreData => DaemonTcp::Data(TcpData {
                    connection_id: id,
                    bytes: Default::default(),
                }),
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
                    DaemonTcp::Close(TcpClose { connection_id: id })
                }
                IncomingStreamItem::Finished(Err(error)) => {
                    self.queued_messages.push_back(DaemonTcp::Close(TcpClose { connection_id: id }));
                    return Ok(DaemonMessage::LogMessage(LogMessage::warn(format!(
                        "Mirrored connection {id} failed: {}",
                        Report::new(error)
                    ))));
                }
            },

            traffic = Self::next(&mut self.mirror_handle, &mut self.ongoing_requests, &self.protocol_version, &self.port_filters) => match traffic? {
                MirroredTraffic::Tcp(tcp) if self.protocol_version.matches(&MODE_AGNOSTIC_HTTP_REQUESTS) => {
                    let id = self.connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;
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
                            .map(|tls| tls.transport_type(&self.protocol_version))
                            .unwrap_or(IncomingTrafficTransportType::Tcp),
                    };
                    self.incoming_streams.insert(id, tcp.stream);
                    DaemonTcp::NewConnectionV2(message)
                }

                MirroredTraffic::Tcp(tcp) => {
                    if tcp.info.tls_connector.is_some() {
                        return Ok(DaemonMessage::LogMessage(LogMessage::error(format!(
                            "A TLS connection was not mirrored due to mirrord-protocol version requirement: {}",
                            *MODE_AGNOSTIC_HTTP_REQUESTS,
                        ))));
                    }

                    if self.port_filters.contains_key(&tcp.info.original_destination.port()) {
                        return Ok(DaemonMessage::LogMessage(LogMessage::warn(
                            "TCP traffic skipped due to HTTP filter on this port".to_owned()
                        )));
                    }

                    let id = self.connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;
                    self.incoming_streams.insert(id, tcp.stream);

                    let message = NewTcpConnectionV1 {
                        connection_id: id,
                        remote_address: tcp.info.peer_addr.ip(),
                        destination_port: tcp.info.original_destination.port(),
                        source_port: tcp.info.peer_addr.port(),
                        local_address: tcp.info.local_addr.ip(),
                    };
                    DaemonTcp::NewConnectionV1(message)
                }

                MirroredTraffic::Http(http) if self.protocol_version.matches(&MODE_AGNOSTIC_HTTP_REQUESTS) => {
                    let id = self.connection_ids_iter.next().ok_or(AgentError::ExhaustedConnectionId)?;

                    self.incoming_streams.insert(id, http.stream);

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
                            .as_ref()
                            .map(|tls| tls.transport_type(&self.protocol_version))
                            .unwrap_or(IncomingTrafficTransportType::Tcp),
                        request: InternalHttpRequest {
                            method: http.request_head.parts.method,
                            uri: http.request_head.parts.uri,
                            headers: http.request_head.parts.headers,
                            version: http.request_head.parts.version,
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
                        *MODE_AGNOSTIC_HTTP_REQUESTS,
                    ))));
                }
            },

            else => std::future::pending().await,
        };

        Ok(DaemonMessage::Tcp(message))
    }
}

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        net::SocketAddr,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use bytes::Bytes;
    use hyper::Request;
    use mirrord_protocol::{
        Payload,
        tcp::{self, Filter, InternalHttpBodyFrame, JsonPathQuery},
    };
    use tokio::{
        sync::{broadcast, oneshot},
        task::JoinSet,
    };
    use tokio_stream::wrappers::BroadcastStream;

    use super::TcpMirrorApi;
    use crate::{
        http::filter::{HttpFilter, MatchResult},
        incoming::{
            BodyBuffering, ConnError, ConnectionInfo, IncomingStream, IncomingStreamItem,
            MirrorHandle, MirroredHttp, MirroredTraffic, RedirectorTask, RedirectorTaskConfig,
            RequestHead, test::DummyRedirector,
        },
        util::protocol_version::ClientProtocolVersion,
    };

    /// The port every request built by [`split_body_request`] is addressed to.
    const PORT: u16 = 8080;

    /// A mirror handle with no mirrored ports, so [`TcpMirrorApi::next`] only ever yields what
    /// the body buffering tasks hand back.
    fn idle_mirror_handle() -> MirrorHandle {
        let (redirector, _state, _conn_tx) = DummyRedirector::new();
        let (_task, _steal_handle, mirror_handle) = RedirectorTask::new(
            redirector,
            Default::default(),
            Default::default(),
            RedirectorTaskConfig::from_env(),
        );
        mirror_handle
    }

    /// Buffers the request's body in the background and runs [`TcpMirrorApi::next`] over the
    /// result, the way the mirror does once a filter asked for the body. Returns the mirrored
    /// request, or `None` when the mirror dropped it.
    ///
    /// `next` only runs once buffering is over, however long it took, so the verdict seen here is
    /// the one the filter gives on the final body. A dropped request leaves `next` with nothing
    /// to yield, which shows as a timeout with the buffering task already consumed.
    async fn next_after_buffering(
        filter: HttpFilter,
        mut http: MirroredHttp,
    ) -> Option<MirroredHttp> {
        let filters = HashMap::from([(PORT, filter)]);
        let version = ClientProtocolVersion::default();
        let mut handle = idle_mirror_handle();
        let mut ongoing = JoinSet::new();
        let (buffered_tx, buffered_rx) = oneshot::channel();
        ongoing.spawn(async move {
            let _ = http.buffer_body().await;
            let _ = buffered_tx.send(());
            http
        });
        buffered_rx.await.unwrap();

        let next = TcpMirrorApi::next(&mut handle, &mut ongoing, &version, &filters);
        match tokio::time::timeout(Duration::from_millis(500), next).await {
            Ok(Ok(MirroredTraffic::Http(http))) => Some(http),
            Ok(other) => panic!("expected a mirrored HTTP request, got {other:?}"),
            Err(_elapsed) => {
                assert!(
                    ongoing.is_empty(),
                    "the buffered request was neither mirrored nor dropped"
                );
                None
            }
        }
    }

    /// A mirrored request whose body arrives in two frames: `first_frame` is already in hand,
    /// `rest` comes later through the connection stream.
    fn split_body_request(
        first_frame: &'static [u8],
        rest: impl IntoIterator<Item = IncomingStreamItem>,
    ) -> MirroredHttp {
        let (tx, rx) = broadcast::channel(8);
        for item in rest {
            tx.send(item).unwrap();
        }

        let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
        let (parts, _) = Request::builder()
            .method("POST")
            .uri("/orders")
            .body(())
            .unwrap()
            .into_parts();

        MirroredHttp {
            info: Arc::new(ConnectionInfo {
                original_destination: addr,
                local_addr: addr,
                peer_addr: addr,
                tls_connector: None,
                passthrough_original_dst: false,
                passthrough_stream: Arc::new(Mutex::new(None)),
            }),
            request_head: RequestHead {
                parts,
                body_head: vec![InternalHttpBodyFrame::Data(Payload(Bytes::from_static(
                    first_frame,
                )))],
                body_finished: false,
            },
            stream: IncomingStream::Mirror(BroadcastStream::new(rx)),
            body_buffering: BodyBuffering::default(),
        }
    }

    fn not_preview_tenant() -> HttpFilter {
        let tcp_filter =
            tcp::HttpFilter::Not(Box::new(tcp::HttpFilter::Body(tcp::HttpBodyFilter::Json {
                query: JsonPathQuery::new("$.tenant".to_owned()).unwrap(),
                matches: Filter::new("preview".to_owned()).unwrap(),
            })));
        HttpFilter::try_from(&tcp_filter).unwrap()
    }

    async fn check(filter: &HttpFilter, http: &mut MirroredHttp) -> MatchResult {
        let (parts, body) = http.parts_and_body();
        filter.matches(parts, body).await
    }

    /// The mirror checks a request against its filter as soon as the head arrives, before the
    /// body is complete. With only part of the body in hand, a `Not` over a body filter must not
    /// read that as "body does not match" and mirror the request right away. The decision waits
    /// for the rest of the body, and once the full body does match the inner filter, the request
    /// is not mirrored.
    #[tokio::test]
    async fn not_over_body_filter_decides_only_on_the_full_body() {
        let filter = not_preview_tenant();

        let mut http = split_body_request(
            br#"{"tenant":"pre"#,
            [
                IncomingStreamItem::Frame(InternalHttpBodyFrame::Data(Payload(
                    Bytes::from_static(br#"view"}"#),
                ))),
                IncomingStreamItem::NoMoreFrames,
            ],
        );
        assert_eq!(check(&filter, &mut http).await, MatchResult::NeedsBody);

        http.buffer_body().await.unwrap();
        assert!(http.request_head.body_finished);
        assert_eq!(check(&filter, &mut http).await, MatchResult::NoMatch);

        // The same split body for another tenant is not preview traffic, so it is mirrored.
        let mut http = split_body_request(
            br#"{"tenant":"li"#,
            [
                IncomingStreamItem::Frame(InternalHttpBodyFrame::Data(Payload(
                    Bytes::from_static(br#"ve"}"#),
                ))),
                IncomingStreamItem::NoMoreFrames,
            ],
        );
        assert_eq!(check(&filter, &mut http).await, MatchResult::NeedsBody);
        http.buffer_body().await.unwrap();
        assert_eq!(check(&filter, &mut http).await, MatchResult::Match);
    }

    /// A body the agent refuses to buffer (here: `Content-Length` over the limit) never becomes
    /// available. The request must not stay undecided after that: the body filter counts as not
    /// matching, so its negation matches. `next_mirrors_a_request_whose_body_buffering_gave_up`
    /// covers the same request going through the mirror loop.
    #[tokio::test]
    async fn not_over_body_filter_matches_once_buffering_gave_up() {
        let filter = not_preview_tenant();

        let mut http = split_body_request(br#"{"tenant":"pre"#, []);
        http.request_head.parts.headers.insert(
            "content-length",
            (1024 * 1024 * 1024).to_string().parse().unwrap(),
        );
        assert_eq!(check(&filter, &mut http).await, MatchResult::NeedsBody);

        http.buffer_body().await.unwrap_err();
        assert!(!http.request_head.body_finished);
        assert_eq!(check(&filter, &mut http).await, MatchResult::Match);
    }

    /// A body that buffering gave up on never finishes, but `next` still shows the request to the
    /// filter, the same way the steal path does. A given-up body matches `not (body ...)`, so the
    /// request is mirrored with its body unfinished.
    #[tokio::test]
    async fn next_mirrors_a_request_whose_body_buffering_gave_up() {
        let mut http = split_body_request(br#"{"tenant":"pre"#, []);
        http.request_head.parts.headers.insert(
            "content-length",
            (1024 * 1024 * 1024).to_string().parse().unwrap(),
        );

        let mirrored = next_after_buffering(not_preview_tenant(), http)
            .await
            .expect("the request is mirrored");
        assert_eq!(mirrored.body_buffering, BodyBuffering::GaveUp);
        assert!(!mirrored.request_head.body_finished);
    }

    /// A connection error while the body is being read is not the same as giving up on a body
    /// that is too big or too slow: there is nothing left to mirror. `not (body ...)` must not
    /// read it as "body does not match" and mirror a request with a broken body.
    #[tokio::test]
    async fn next_drops_a_request_whose_body_broke_even_under_not() {
        let http = split_body_request(
            br#"{"tenant":"pre"#,
            [IncomingStreamItem::Finished(Err(ConnError::AgentBug(
                "client went away".to_owned(),
            )))],
        );

        assert!(
            next_after_buffering(not_preview_tenant(), http)
                .await
                .is_none()
        );
    }

    /// A body that the mirror does buffer in full is decided through `next` the same way it is
    /// decided by hand: preview traffic is dropped, everything else is mirrored.
    #[tokio::test]
    async fn next_decides_a_fully_buffered_body() {
        let preview = split_body_request(
            br#"{"tenant":"pre"#,
            [
                IncomingStreamItem::Frame(InternalHttpBodyFrame::Data(Payload(
                    Bytes::from_static(br#"view"}"#),
                ))),
                IncomingStreamItem::NoMoreFrames,
            ],
        );
        assert!(
            next_after_buffering(not_preview_tenant(), preview)
                .await
                .is_none()
        );

        let live = split_body_request(
            br#"{"tenant":"li"#,
            [
                IncomingStreamItem::Frame(InternalHttpBodyFrame::Data(Payload(
                    Bytes::from_static(br#"ve"}"#),
                ))),
                IncomingStreamItem::NoMoreFrames,
            ],
        );
        let mirrored = next_after_buffering(not_preview_tenant(), live)
            .await
            .expect("the request is mirrored");
        assert!(mirrored.request_head.body_finished);
    }

    /// A filter that never looks at the body answers on the head alone, so a partial body does
    /// not delay mirroring and a non-matching request is dropped right away.
    #[tokio::test]
    async fn header_filters_decide_on_the_head_without_waiting_for_the_body() {
        let tcp_filter = tcp::HttpFilter::Not(Box::new(tcp::HttpFilter::Header(
            Filter::new("baggage: .*mirrord=preview-key".to_owned()).unwrap(),
        )));
        let filter = HttpFilter::try_from(&tcp_filter).unwrap();

        let mut http = split_body_request(b"partial", []);
        assert_eq!(check(&filter, &mut http).await, MatchResult::Match);

        let mut http = split_body_request(b"partial", []);
        http.request_head
            .parts
            .headers
            .insert("baggage", "mirrord=preview-key".parse().unwrap());
        assert_eq!(check(&filter, &mut http).await, MatchResult::NoMatch);
    }
}

use std::{num::NonZeroUsize, ops::Not, time::Duration};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use http_body::Frame;
use http_body_util::{BodyExt, Empty, Full, StreamBody, combinators::BoxBody};
use hyper::{HeaderMap, StatusCode};
use mirrord_protocol::{
    ClientMessage, Payload,
    tcp::{
        ChunkedRequestBodyV1, ChunkedResponse, HttpResponse, InternalHttpBodyFrame,
        InternalHttpBodyNew, InternalHttpRequest, InternalHttpResponse, LayerTcpSteal, TcpData,
    },
};
use rstest::rstest;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    client::tunnels::{TrafficTunnels, TunnelId, TunnelType},
    traffic::{TunneledData, TunneledRequest},
};

const PAYLOAD: Bytes = Bytes::from_static(b"hello");

async fn assert_tunnels_idle(tunnels: &mut TrafficTunnels) {
    assert_eq!(tunnels.next().await, None);
    assert!(tunnels.tunnels.iter().all(|(_, map)| map.is_empty()));
}

/// Verifies that [`TrafficTunnels`] correctly handles raw connections' data and shutdowns.
#[rstest]
#[case(TunnelType::OutgoingTcp, true)]
#[case(TunnelType::OutgoingUdp, true)]
#[case(TunnelType::IncomingSteal, true)]
#[case(TunnelType::IncomingMirror, false)]
#[tokio::test]
async fn raw_connection(#[case] tunnel_type: TunnelType, #[case] has_outbound: bool) {
    assert_eq!(tunnel_type.has_outbound(), has_outbound);

    let mut tunnels = TrafficTunnels::new(
        NonZeroUsize::MAX,
        Duration::MAX,
        &*mirrord_protocol::VERSION,
    );
    let id = TunnelId(tunnel_type, 0);

    let TunneledData {
        mut sink,
        mut stream,
        ..
    } = tunnels.server_connection(id).unwrap();

    if has_outbound {
        sink.send(PAYLOAD.clone()).await.unwrap();
        assert_eq!(
            tunnels.next().await.unwrap(),
            id.write_message(PAYLOAD.clone()).unwrap(),
        );
        sink.send(PAYLOAD.clone()).await.unwrap();
        assert_eq!(
            tunnels.next().await.unwrap(),
            id.write_message(PAYLOAD.clone()).unwrap(),
        );
        drop(sink);
        assert_eq!(
            tunnels.next().await.unwrap(),
            id.write_message(Default::default()).unwrap(),
        );
    } else {
        sink.send(PAYLOAD).await.unwrap_err();
    }

    assert!(tunnels.server_data(id, PAYLOAD.clone()).await.is_empty());
    assert_eq!(stream.next().await.unwrap(), PAYLOAD);

    assert!(tunnels.server_data(id, PAYLOAD.clone()).await.is_empty());
    assert_eq!(stream.next().await.unwrap(), PAYLOAD);

    assert_eq!(tunnels.server_shutdown(id), id.close_message());

    assert_tunnels_idle(&mut tunnels).await;
}

/// Verifies that [`TrafficTunnels`] correctly handles the case where the server
/// sends connection data, but the client is no longer receiving.
#[tokio::test]
async fn raw_connection_write_after_close() {
    let mut tunnels = TrafficTunnels::new(
        NonZeroUsize::MAX,
        Duration::MAX,
        &*mirrord_protocol::VERSION,
    );
    let id = TunnelId(TunnelType::IncomingSteal, 0);

    let tunnel = tunnels.server_connection(id).unwrap();
    drop(tunnel.stream);
    assert_eq!(
        tunnels.server_data(id, PAYLOAD.clone()).await,
        ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(id.1)),
    );
    assert_tunnels_idle(&mut tunnels).await;
}

/// Verifies that [`TrafficTunnels`] correctly handles successful HTTP exchanges
/// with no HTTP upgrade.
#[rstest]
#[case(true, false, false)]
#[case(true, false, true)]
#[case(true, true, false)]
#[case(true, true, true)]
#[case(false, true, false)]
#[case(false, false, false)]
#[tokio::test]
async fn simple_request(
    #[case] stolen: bool,
    #[case] streaming_request_body: bool,
    #[case] streaming_response_body: bool,
) {
    let mut tunnels = TrafficTunnels::new(
        NonZeroUsize::MAX,
        Duration::MAX,
        &*mirrord_protocol::VERSION,
    );
    let tunnel_type = if stolen {
        TunnelType::IncomingSteal
    } else {
        TunnelType::IncomingMirror
    };
    let id = TunnelId(tunnel_type, 0);

    let request = InternalHttpRequest {
        method: hyper::Method::GET,
        uri: "/".parse().unwrap(),
        headers: Default::default(),
        version: hyper::Version::HTTP_11,
        body: InternalHttpBodyNew {
            frames: vec![InternalHttpBodyFrame::Data(Payload(PAYLOAD.clone()))],
            is_last: streaming_request_body.not(),
        },
    };
    let TunneledRequest {
        mut request,
        response_tx,
        mut upgrade_rx,
    } = tunnels.server_request(id, 80, request).unwrap();

    assert_eq!(request.method(), hyper::Method::GET);
    assert_eq!(request.uri(), "/");
    assert_eq!(request.version(), hyper::Version::HTTP_11);
    assert!(request.headers().is_empty());

    assert_eq!(
        request
            .body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap(),
        PAYLOAD,
    );

    if streaming_request_body {
        assert!(
            tunnels
                .server_request_body(
                    id,
                    InternalHttpBodyNew {
                        frames: vec![InternalHttpBodyFrame::Data(Payload(PAYLOAD.clone()))],
                        is_last: false,
                    },
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            request
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap(),
            PAYLOAD,
        );

        assert!(
            tunnels
                .server_request_body(
                    id,
                    InternalHttpBodyNew {
                        frames: vec![InternalHttpBodyFrame::Trailers(Default::default())],
                        is_last: true,
                    },
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            request
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_trailers()
                .unwrap(),
            HeaderMap::default(),
        );
    }

    match (stolen, streaming_response_body) {
        (true, true) => {
            let (body_tx, body_rx) = mpsc::channel(8);
            let body = BoxBody::new(StreamBody::new(ReceiverStream::new(body_rx)));
            let response = hyper::Response::new(body);
            response_tx.send(response).unwrap();
            assert_eq!(
                tunnels.next().await.unwrap(),
                ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                    ChunkedResponse::Start(HttpResponse {
                        port: 80,
                        connection_id: 0,
                        request_id: 0,
                        internal_response: InternalHttpResponse {
                            status: hyper::StatusCode::OK,
                            version: hyper::Version::HTTP_11,
                            headers: Default::default(),
                            body: Default::default(),
                        },
                    })
                )),
            );

            body_tx
                .send(Ok(Frame::data(PAYLOAD.clone())))
                .await
                .unwrap();
            assert_eq!(
                tunnels.next().await.unwrap(),
                ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                    ChunkedRequestBodyV1 {
                        connection_id: 0,
                        request_id: 0,
                        frames: vec![InternalHttpBodyFrame::Data(Payload(PAYLOAD.clone()))],
                        is_last: false,
                    }
                ))),
            );

            body_tx
                .send(Ok(Frame::trailers(Default::default())))
                .await
                .unwrap();
            assert_eq!(
                tunnels.next().await.unwrap(),
                ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                    ChunkedRequestBodyV1 {
                        connection_id: 0,
                        request_id: 0,
                        frames: vec![InternalHttpBodyFrame::Trailers(Default::default())],
                        is_last: false,
                    }
                ))),
            );

            drop(body_tx);
            assert_eq!(
                tunnels.next().await.unwrap(),
                [
                    ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                        ChunkedResponse::Body(ChunkedRequestBodyV1 {
                            connection_id: 0,
                            request_id: 0,
                            frames: vec![],
                            is_last: true,
                        })
                    )),
                    ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(0)),
                ],
            );

            upgrade_rx.await.unwrap_err();
        }

        (true, false) => {
            let response =
                hyper::Response::new(BoxBody::new(Full::new(PAYLOAD.clone()).map_err(|_| ())));
            response_tx.send(response).unwrap();
            assert_eq!(
                tunnels.next().await.unwrap(),
                [
                    ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                        ChunkedResponse::Start(HttpResponse {
                            port: 80,
                            connection_id: 0,
                            request_id: 0,
                            internal_response: InternalHttpResponse {
                                status: hyper::StatusCode::OK,
                                version: hyper::Version::HTTP_11,
                                headers: Default::default(),
                                body: vec![InternalHttpBodyFrame::Data(Payload(PAYLOAD.clone()))],
                            },
                        })
                    )),
                    ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                        ChunkedResponse::Body(ChunkedRequestBodyV1 {
                            frames: vec![],
                            is_last: true,
                            connection_id: 0,
                            request_id: 0,
                        })
                    )),
                    ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(0)),
                ],
            );
            upgrade_rx.await.unwrap_err();
        }

        (false, _) => {
            assert!(response_tx.is_closed());
            assert!(tunnels.next().await.is_none());
            tokio::time::timeout(Duration::from_millis(100), &mut upgrade_rx)
                .await
                .unwrap_err();
            tunnels.server_close(id);
        }
    }

    assert_tunnels_idle(&mut tunnels).await;
}

/// Verifies that [`TrafficTunnels`] correctly handles stolen HTTP upgrades,
/// with and without early data.
#[rstest]
#[tokio::test]
async fn http_upgrade_stolen(
    #[values(true, false)] switching_protocols: bool,
    #[values(true, false)] early_data: bool,
    #[values(true, false)] early_shutdown: bool,
) {
    let mut tunnels = TrafficTunnels::new(
        NonZeroUsize::MAX,
        Duration::MAX,
        &*mirrord_protocol::VERSION,
    );
    let id = TunnelId(TunnelType::IncomingSteal, 0);

    let TunneledRequest {
        response_tx,
        upgrade_rx,
        ..
    } = tunnels
        .server_request(
            id,
            2137,
            InternalHttpRequest {
                method: hyper::Method::GET,
                uri: "/".parse().unwrap(),
                headers: Default::default(),
                version: hyper::Version::HTTP_11,
                body: InternalHttpBodyNew {
                    frames: Default::default(),
                    is_last: true,
                },
            },
        )
        .unwrap();

    if early_data {
        assert!(tunnels.server_data(id, PAYLOAD.clone()).await.is_empty());
    }

    if early_shutdown {
        assert!(tunnels.server_shutdown(id).is_empty());
    }

    let mut response = hyper::Response::new(BoxBody::new(Empty::<Bytes>::new().map_err(|_| ())));
    if switching_protocols {
        *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    }
    response_tx.send(response).unwrap();

    let out = tunnels.next().await.unwrap();

    assert_eq!(
        &out.as_slice()[..2],
        [
            ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Start(
                HttpResponse {
                    port: 2137,
                    request_id: 0,
                    connection_id: 0,
                    internal_response: InternalHttpResponse {
                        status: if switching_protocols {
                            StatusCode::SWITCHING_PROTOCOLS
                        } else {
                            StatusCode::OK
                        },
                        version: hyper::Version::HTTP_11,
                        headers: Default::default(),
                        body: vec![],
                    },
                }
            ))),
            ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                ChunkedRequestBodyV1 {
                    request_id: 0,
                    connection_id: 0,
                    frames: Default::default(),
                    is_last: true,
                }
            )))
        ],
    );

    if switching_protocols {
        assert_eq!(out.len(), 2);
    } else {
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[2],
            ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(0)),
        );
        assert_tunnels_idle(&mut tunnels).await;
        return;
    }

    let mut tunnel = upgrade_rx.await.unwrap();

    tunnel.sink.send(PAYLOAD.clone()).await.unwrap();
    assert_eq!(
        tunnels.next().await.unwrap(),
        ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
            connection_id: 0,
            bytes: Payload(PAYLOAD.clone()),
        })),
    );

    if early_shutdown.not() {
        assert!(tunnels.server_data(id, PAYLOAD.clone()).await.is_empty());
        assert!(tunnels.server_shutdown(id).is_empty());
    }
    let received_data = tunnel.stream.collect::<Vec<_>>().await;
    let expect = match (early_data, early_shutdown) {
        (true, true) => vec![PAYLOAD.clone()],
        (true, false) => vec![PAYLOAD.clone(), PAYLOAD.clone()],
        (false, true) => vec![],
        (false, false) => vec![PAYLOAD.clone()],
    };
    assert_eq!(received_data, expect,);

    drop(tunnel.sink);
    assert_eq!(
        tunnels.next().await.unwrap(),
        ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(0)),
    );
    assert_tunnels_idle(&mut tunnels).await;
}

/// Verifies that [`TrafficTunnels`] correctly handles mirrord HTTP upgrades.
#[rstest]
#[tokio::test]
async fn http_upgrade_mirror(#[values(0, 1, 2)] data_chunks: usize) {
    let mut tunnels = TrafficTunnels::new(
        NonZeroUsize::MAX,
        Duration::MAX,
        &*mirrord_protocol::VERSION,
    );
    let id = TunnelId(TunnelType::IncomingMirror, 0);

    let TunneledRequest { upgrade_rx, .. } = tunnels
        .server_request(
            id,
            2137,
            InternalHttpRequest {
                method: hyper::Method::GET,
                uri: "/".parse().unwrap(),
                headers: Default::default(),
                version: hyper::Version::HTTP_11,
                body: InternalHttpBodyNew {
                    frames: Default::default(),
                    is_last: true,
                },
            },
        )
        .unwrap();

    for _ in 0..data_chunks {
        assert!(tunnels.server_data(id, PAYLOAD.clone()).await.is_empty());
    }

    assert_eq!(tunnels.server_shutdown(id), id.close_message());

    let tunnel = upgrade_rx.await.unwrap();
    let received_data = tunnel.stream.collect::<Vec<_>>().await;
    assert_eq!(
        received_data,
        std::iter::repeat(PAYLOAD.clone())
            .take(data_chunks)
            .collect::<Vec<_>>(),
    );

    assert_tunnels_idle(&mut tunnels).await;
}

/// Verifies that [`TrafficTunnels`] correctly handles HTTP request body errors.
#[rstest]
#[tokio::test]
async fn http_request_body_error(#[values(true, false)] stolen: bool) {
    let mut tunnels = TrafficTunnels::new(
        NonZeroUsize::MAX,
        Duration::MAX,
        &*mirrord_protocol::VERSION,
    );
    let tunnel_type = if stolen {
        TunnelType::IncomingSteal
    } else {
        TunnelType::IncomingMirror
    };
    let id = TunnelId(tunnel_type, 0);

    let TunneledRequest {
        mut request,
        response_tx: _response_tx,
        upgrade_rx: _upgrade_rx,
    } = tunnels
        .server_request(
            id,
            2137,
            InternalHttpRequest {
                method: hyper::Method::GET,
                uri: "/".parse().unwrap(),
                headers: Default::default(),
                version: hyper::Version::HTTP_11,
                body: InternalHttpBodyNew {
                    frames: Default::default(),
                    is_last: false,
                },
            },
        )
        .unwrap();
    assert_eq!(
        tunnels
            .server_request_body_error(id, Some("oh no".into()))
            .await
            .unwrap(),
        id.close_message(),
    );

    assert_eq!(
        request.body_mut().frame().await.unwrap().unwrap_err().0,
        Some("oh no".to_string()),
    );

    assert_tunnels_idle(&mut tunnels).await;
}

/// Verifies that [`TrafficTunnels`] correctly handles the case
/// where the server sends request body frames,
/// but the client no is longer receiving.
///
/// The server should still get the response.
#[tokio::test]
async fn http_request_body_after_close() {
    let mut tunnels = TrafficTunnels::new(
        NonZeroUsize::MAX,
        Duration::MAX,
        &*mirrord_protocol::VERSION,
    );
    let id = TunnelId(TunnelType::IncomingSteal, 0);

    let request = tunnels
        .server_request(
            id,
            2137,
            InternalHttpRequest {
                method: hyper::Method::GET,
                uri: "/".parse().unwrap(),
                headers: Default::default(),
                version: hyper::Version::HTTP_11,
                body: InternalHttpBodyNew {
                    frames: vec![InternalHttpBodyFrame::Data(Payload(PAYLOAD.clone()))],
                    is_last: false,
                },
            },
        )
        .unwrap();

    drop(request.request);
    drop(request.upgrade_rx);
    request
        .response_tx
        .send(hyper::Response::new(BoxBody::new(
            Empty::<Bytes>::new().map_err(|_| ()),
        )))
        .unwrap();

    assert!(
        tunnels
            .server_request_body(
                id,
                InternalHttpBodyNew {
                    frames: vec![InternalHttpBodyFrame::Data(Payload(PAYLOAD.clone()))],
                    is_last: true,
                },
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        tunnels.next().await.unwrap(),
        [
            ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Start(
                HttpResponse {
                    port: 2137,
                    connection_id: 0,
                    request_id: 0,
                    internal_response: InternalHttpResponse {
                        status: StatusCode::OK,
                        version: hyper::Version::HTTP_11,
                        headers: Default::default(),
                        body: vec![],
                    }
                }
            ))),
            ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                ChunkedRequestBodyV1 {
                    frames: vec![],
                    is_last: true,
                    connection_id: 0,
                    request_id: 0
                }
            ))),
            ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(0)),
        ],
    );

    assert_tunnels_idle(&mut tunnels).await;
}

use std::{
    net::{Ipv4Addr, SocketAddr},
    num::NonZeroUsize,
    ops::Not,
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse, GetEnvVarsRequest, RemoteEnvVars,
    dns::{
        AddressFamily, DnsLookup, GetAddrInfoRequest, GetAddrInfoRequestV2, GetAddrInfoResponse,
        LookupRecord, ReverseDnsLookupRequest, ReverseDnsLookupResponse, SockType,
    },
    file::{
        AccessFileRequest, AccessFileResponse, CloseDirRequest, CloseFileRequest, FdOpenDirRequest,
        FsMetadataInternal, FsMetadataInternalV2, OpenDirResponse, OpenFileRequest,
        OpenFileResponse, OpenOptionsInternal, ReadDirRequest, ReadDirResponse, STATFS_V2_VERSION,
        STATFS_VERSION, StatFsRequestV2, XstatFsResponse, XstatFsResponseV2,
    },
    outgoing::{
        DaemonConnect, DaemonConnectV2,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
    },
    tcp::{DaemonTcp, LayerTcpSteal, NewTcpConnectionV1, StealType},
};
use rstest::rstest;

use crate::client::{
    ClientConfig, ClientError, MirrordClient, MirrordClientRetry, error::TaskError,
    incoming::IncomingMode, outgoing::OutgoingMode, test::connector::TestConnector,
};

mod connector;

/// Verifies [`GetAddrInfoRequestV2`] handling.
#[rstest]
#[tokio::test]
async fn dns(#[values(true, false)] downgraded: bool) {
    let protocol_version = if downgraded {
        "1.14.0".parse().unwrap()
    } else {
        mirrord_protocol::VERSION.clone()
    };
    let request = GetAddrInfoRequestV2 {
        node: "hello".into(),
        service_port: 80,
        family: AddressFamily::Ipv4Only,
        socktype: SockType::Stream,
        flags: 0,
        protocol: 0,
    };

    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(*client.protocol_version(), protocol_version);
        let response = client.make_request(request.clone()).await.unwrap();
        assert_eq!(
            response,
            DnsLookup(vec![LookupRecord {
                name: "hello".into(),
                ip: Ipv4Addr::new(1, 2, 3, 4).into(),
            }]),
        );
    };

    let server_fut = async {
        let mut server = acceptor.accept(protocol_version.clone()).await;
        match server.stream.next().await.unwrap() {
            ClientMessage::GetAddrInfoRequest(req) if downgraded => {
                assert_eq!(
                    req,
                    GetAddrInfoRequest {
                        node: "hello".into()
                    }
                );
            }
            ClientMessage::GetAddrInfoRequestV2(req) if downgraded.not() => {
                assert_eq!(req, request);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        server
            .sink
            .send(Ok(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(
                Ok(DnsLookup(vec![LookupRecord {
                    name: "hello".into(),
                    ip: Ipv4Addr::new(1, 2, 3, 4).into(),
                }])),
            ))))
            .await
            .unwrap();
    };

    tokio::join!(client_fut, server_fut);
}

/// Verifies [`GetEnvVarsRequest`] handling.
#[tokio::test]
async fn env_vars() {
    let request = GetEnvVarsRequest {
        env_vars_select: ["*".to_string()].into(),
        env_vars_filter: Default::default(),
    };
    let env_vars = RemoteEnvVars(Default::default());

    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32 * 1024).unwrap(),
        )
        .await
        .unwrap();
        let response = client.make_request(request.clone()).await.unwrap();
        assert_eq!(response, env_vars,);
    };

    let server_fut = async {
        let mut server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;
        match server.stream.next().await.unwrap() {
            ClientMessage::GetEnvVarsRequest(got_request) => assert_eq!(got_request, request),
            other => panic!("unexpected message: {other:?}"),
        }
        server
            .sink
            .send(Ok(DaemonMessage::GetEnvVarsResponse(Ok(env_vars.clone()))))
            .await
            .unwrap();
    };

    tokio::join!(client_fut, server_fut);
}

/// Verifies [`ReverseDnsLookupRequest`] handling.
#[rstest]
#[tokio::test]
async fn reverse_dns(#[values(true, false)] supported: bool) {
    let protocol_version = if supported {
        mirrord_protocol::VERSION.clone()
    } else {
        "1.14.0".parse().unwrap()
    };
    let request = ReverseDnsLookupRequest {
        ip_address: Ipv4Addr::new(2, 1, 3, 7).into(),
    };

    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(*client.protocol_version(), protocol_version);
        let result = client.make_request(request.clone()).await;
        match (result, supported) {
            (Ok(name), true) => assert_eq!(name, "hello"),
            (Err(ClientError::NotSupported), false) => {}
            other => panic!("unexpected request result: {other:?}"),
        }
    };

    let server_fut = async {
        let mut server = acceptor.accept(protocol_version.clone()).await;
        match server.stream.next().await {
            Some(ClientMessage::ReverseDnsLookup(got_request)) if supported => {
                assert_eq!(got_request, request);
                server
                    .sink
                    .send(Ok(DaemonMessage::ReverseDnsLookup(Ok(
                        ReverseDnsLookupResponse {
                            hostname: Ok("hello".into()),
                        },
                    ))))
                    .await
                    .unwrap();
                assert!(server.stream.next().await.is_none());
            }
            None if supported.not() => {}
            other => panic!("unexpected message: {other:?}"),
        }
    };

    tokio::join!(client_fut, server_fut);
}

/// Verifies handling of multiple file requests,
/// some requiring a response and some not.
#[tokio::test]
async fn file_ops() {
    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32).unwrap(),
        )
        .await
        .unwrap();
        let OpenFileResponse { fd } = client
            .make_request(OpenFileRequest {
                path: "/some/file".into(),
                open_options: OpenOptionsInternal::default(),
            })
            .await
            .unwrap();
        let OpenDirResponse { fd: dir_fd } = client
            .make_request(FdOpenDirRequest { remote_fd: fd })
            .await
            .unwrap();
        client
            .make_request_no_response(CloseFileRequest { fd })
            .await;
        let _ = client
            .make_request(ReadDirRequest { remote_fd: dir_fd })
            .await
            .unwrap();
        client
            .make_request_no_response(CloseDirRequest { remote_fd: dir_fd })
            .await;
        let _ = client
            .make_request(AccessFileRequest {
                pathname: "/some/file".into(),
                mode: 0,
            })
            .await
            .unwrap();
    };

    let server_fut = async {
        let mut server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;
        match server.stream.next().await.unwrap() {
            ClientMessage::FileRequest(FileRequest::Open(_)) => {}
            other => panic!("unexpected message: {other:?}"),
        }
        server
            .sink
            .send(Ok(DaemonMessage::File(FileResponse::Open(Ok(
                OpenFileResponse { fd: 0 },
            )))))
            .await
            .unwrap();
        match server.stream.next().await.unwrap() {
            ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest {
                remote_fd: 0,
            })) => {}
            other => panic!("unexpected message: {other:?}"),
        }
        server
            .sink
            .send(Ok(DaemonMessage::File(FileResponse::OpenDir(Ok(
                OpenDirResponse { fd: 1 },
            )))))
            .await
            .unwrap();
        match server.stream.next().await.unwrap() {
            ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd: 0 })) => {}
            other => panic!("unxpected message: {other:?}"),
        }
        match server.stream.next().await.unwrap() {
            ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { remote_fd: 1 })) => {}
            other => panic!("unexpected message: {other:?}"),
        }
        server
            .sink
            .send(Ok(DaemonMessage::File(FileResponse::ReadDir(Ok(
                ReadDirResponse { direntry: None },
            )))))
            .await
            .unwrap();
        match server.stream.next().await.unwrap() {
            ClientMessage::FileRequest(FileRequest::CloseDir(CloseDirRequest { remote_fd: 1 })) => {
            }
            other => panic!("unexpected message: {other:?}"),
        }
        match server.stream.next().await.unwrap() {
            ClientMessage::FileRequest(FileRequest::Access(..)) => {}
            other => panic!("unexpected message: {other:?}"),
        }
        server
            .sink
            .send(Ok(DaemonMessage::File(FileResponse::Access(Ok(
                AccessFileResponse,
            )))))
            .await
            .unwrap();
        assert!(server.stream.next().await.is_none());
    };

    tokio::join!(client_fut, server_fut);
}

/// Verifies [`StatFsRequestV2`] handling (most complex file request in terms of downgrading).
#[rstest]
#[case(
    "1.18.0",
    Some(XstatFsResponseV2 { metadata: FsMetadataInternalV2 {
        filesystem_type: 1,
        block_size: 2,
        blocks: 3,
        blocks_free: 4,
        blocks_available: 5,
        files: 6,
        files_free: 7,
        filesystem_id: [8, 8],
        name_len: 9,
        fragment_size: 10,
        flags: 11,
    }}))]
#[case(
    "1.17.0",
    Some(XstatFsResponseV2 { metadata: FsMetadataInternalV2 {
        filesystem_type: 1,
        block_size: 2,
        blocks: 3,
        blocks_free: 4,
        blocks_available: 5,
        files: 6,
        files_free: 7,
        filesystem_id: [0, 0],
        name_len: 0,
        fragment_size: 0,
        flags: 0,
    }}))]
#[case("1.15.0", None)]
#[tokio::test]
async fn file_ops_compat(
    #[case] server_protocol_version: &str,
    #[case] expected_response: Option<XstatFsResponseV2>,
) {
    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32).unwrap(),
        )
        .await
        .unwrap();
        let result = client
            .make_request(StatFsRequestV2 {
                path: Default::default(),
            })
            .await;
        match (result, expected_response) {
            (Ok(response), Some(expected_response)) => assert_eq!(response, expected_response),
            (Err(ClientError::NotSupported), None) => {}
            other => panic!("unexpected file ops outpud: {other:?}"),
        }
    };

    let server_fut = async {
        let version = server_protocol_version.parse::<semver::Version>().unwrap();
        let mut server = acceptor.accept(version.clone()).await;

        if STATFS_V2_VERSION.matches(&version) {
            match server.stream.next().await.unwrap() {
                ClientMessage::FileRequest(FileRequest::StatFsV2(_)) => {}
                other => panic!("unexpected message: {other:?}"),
            }
            server
                .sink
                .send(Ok(DaemonMessage::File(FileResponse::XstatFsV2(Ok(
                    XstatFsResponseV2 {
                        metadata: FsMetadataInternalV2 {
                            filesystem_type: 1,
                            block_size: 2,
                            blocks: 3,
                            blocks_free: 4,
                            blocks_available: 5,
                            files: 6,
                            files_free: 7,
                            filesystem_id: [8, 8],
                            name_len: 9,
                            fragment_size: 10,
                            flags: 11,
                        },
                    },
                )))))
                .await
                .unwrap();
        } else if STATFS_VERSION.matches(&version) {
            match server.stream.next().await.unwrap() {
                ClientMessage::FileRequest(FileRequest::StatFs(_)) => {}
                other => panic!("unexpected message: {other:?}"),
            }
            server
                .sink
                .send(Ok(DaemonMessage::File(FileResponse::XstatFs(Ok(
                    XstatFsResponse {
                        metadata: FsMetadataInternal {
                            filesystem_type: 1,
                            block_size: 2,
                            blocks: 3,
                            blocks_free: 4,
                            blocks_available: 5,
                            files: 6,
                            files_free: 7,
                        },
                    },
                )))))
                .await
                .unwrap();
        }
        assert!(server.stream.next().await.is_none());
    };

    tokio::join!(client_fut, server_fut);
}

/// Verifies behavior of [`MirrordClient`] when the connection to the server is lost.
#[rstest]
#[tokio::test]
async fn connection_lost(#[values(true, false)] can_reconnect: bool) {
    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32).unwrap(),
        )
        .await
        .unwrap();

        let mut results = tokio::join!(
            client.make_request(GetEnvVarsRequest {
                env_vars_filter: Default::default(),
                env_vars_select: Default::default(),
            }),
            client.connect_ip("127.0.0.1:2137".parse().unwrap(), OutgoingMode::Tcp),
            client.make_request(OpenFileRequest {
                path: "/hello".into(),
                open_options: Default::default()
            }),
        );

        if can_reconnect {
            match results {
                (
                    Err(ClientError::ConnectionLost(TaskError::ServerClosed(None))),
                    Err(ClientError::ConnectionLost(TaskError::ServerClosed(None))),
                    Err(ClientError::ConnectionLost(TaskError::ServerClosed(None))),
                ) => {}
                other => panic!("unexpected results: {other:?}"),
            }
            results = tokio::join!(
                client.make_request(GetEnvVarsRequest {
                    env_vars_filter: Default::default(),
                    env_vars_select: Default::default(),
                }),
                client.connect_ip("127.0.0.1:2137".parse().unwrap(), OutgoingMode::Tcp),
                client.make_request(OpenFileRequest {
                    path: "/hello".into(),
                    open_options: Default::default()
                }),
            );
        }

        match results {
            (
                Err(ClientError::TaskFailed(TaskError::ServerClosed(None))),
                Err(ClientError::TaskFailed(TaskError::ServerClosed(None))),
                Err(ClientError::TaskFailed(TaskError::ServerClosed(None))),
            ) => {}
            other => panic!("unexpected results: {other:?}"),
        }
    };

    let server_fut = async {
        let mut server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;
        let mut acceptor = if can_reconnect {
            Some(acceptor)
        } else {
            drop(acceptor);
            None
        };

        loop {
            for _ in 0..3 {
                match server.stream.next().await.unwrap() {
                    ClientMessage::GetEnvVarsRequest(..) => {}
                    ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(..)) => {}
                    ClientMessage::FileRequest(FileRequest::Open(..)) => {}
                    other => panic!("unexpected message: {other:?}"),
                }
            }
            drop(server);
            let Some(mut acceptor) = acceptor.take() else {
                break;
            };
            server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;
        }
    };

    tokio::join!(client_fut, server_fut);
}

/// Verifies that [`MirrordClientRetry`] methods correctly retry requests after reconnects.
#[tokio::test]
async fn retrying_requests() {
    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32).unwrap(),
        )
        .await
        .unwrap();

        tokio::try_join!(
            client.connect_ip_retry(
                "127.0.0.1:2137".parse().unwrap(),
                OutgoingMode::Tcp,
                Duration::from_millis(500),
            ),
            client.make_request_retry(
                GetAddrInfoRequest {
                    node: "localhost".into(),
                },
                Duration::from_millis(500),
            ),
        )
        .unwrap();
    };

    let server_fut = async {
        let mut server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;

        for _ in 0..2 {
            match server.stream.next().await.unwrap() {
                ClientMessage::GetAddrInfoRequest(..) => {}
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(..)) => {}
                other => panic!("unexpected message: {other:?}"),
            }
        }
        drop(server);
        server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;

        for _ in 0..2 {
            match server.stream.next().await.unwrap() {
                ClientMessage::GetAddrInfoRequest(request) => {
                    server
                        .sink
                        .send(Ok(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(
                            Ok(DnsLookup(vec![LookupRecord {
                                name: request.node,
                                ip: Ipv4Addr::LOCALHOST.into(),
                            }])),
                        ))))
                        .await
                        .unwrap();
                }
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(connect)) => {
                    server
                        .sink
                        .send(Ok(DaemonMessage::TcpOutgoing(
                            DaemonTcpOutgoing::ConnectV2(DaemonConnectV2 {
                                uid: connect.uid,
                                connect: Ok(DaemonConnect {
                                    connection_id: 0,
                                    remote_address: connect.remote_address,
                                    local_address: "127.0.0.1:2136"
                                        .parse::<SocketAddr>()
                                        .unwrap()
                                        .into(),
                                }),
                            }),
                        )))
                        .await
                        .unwrap();
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
    };

    tokio::join!(client_fut, server_fut);
}

/// Verifies that [`MirrordClientRetry`] methods correctly retries port subscriptions.
#[tokio::test]
async fn retrying_port_subscription() {
    let (connector, mut acceptor) = TestConnector::new_pair();

    let client_fut = async {
        let client = MirrordClient::new(
            connector,
            ClientConfig::cli(),
            NonZeroUsize::new(32).unwrap(),
        )
        .await
        .unwrap();

        let mut subscription =
            client.subscribe_port_retry(80, IncomingMode::Steal, None, Duration::from_millis(500));

        let traffic = subscription.next().await.unwrap().unwrap();
        assert_eq!(traffic.remote_peer_addr, "127.0.0.1:1".parse().unwrap());

        let traffic = subscription.next().await.unwrap().unwrap();
        assert_eq!(traffic.remote_peer_addr, "127.0.0.1:2".parse().unwrap());

        subscription.next().await.unwrap().unwrap_err();
    };

    let server_fut = async {
        let mut server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;

        for i in 1..=2 {
            match server.stream.next().await.unwrap() {
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(80))) => {}
                other => panic!("unexpected message: {other:?}"),
            }
            server
                .sink
                .send(Ok(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
                    80,
                )))))
                .await
                .unwrap();
            server
                .sink
                .send(Ok(DaemonMessage::TcpSteal(DaemonTcp::NewConnectionV1(
                    NewTcpConnectionV1 {
                        connection_id: 0,
                        remote_address: Ipv4Addr::LOCALHOST.into(),
                        destination_port: 80,
                        local_address: Ipv4Addr::LOCALHOST.into(),
                        source_port: i,
                    },
                ))))
                .await
                .unwrap();
            drop(server);
            server = acceptor.accept(mirrord_protocol::VERSION.clone()).await;
        }
    };

    tokio::join!(client_fut, server_fut);
}

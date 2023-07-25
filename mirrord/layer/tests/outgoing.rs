#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    file::{MetadataInternal, XstatResponse},
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerClose, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
};
use rstest::rstest;

mod common;

pub use common::*;
use futures::{SinkExt, StreamExt, TryStreamExt};

// TODO: add a test for when DNS lookup is unsuccessful, to make sure the layer returns a valid
//      error to the user application.

/// Test outgoing UDP.
/// Application, for each remote peer in [`RUST_OUTGOING_PEERS`]:
/// 1. Opens a UDP port at [`RUST_OUTGOING_LOCAL`]
/// 2. Connects to the remote peer
/// 3. Sends some data
/// 4. Expects the peer to send the same data back
///
/// # Ignored
/// This test is ignored due to a bug - `recv_from` call returns an invalid remote peer address.
#[ignore]
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn outgoing_udp(dylib_path: &PathBuf) {
    let (mut test_process, layer_connection) = Application::RustOutgoingUdp
        .start_process_with_layer(dylib_path, vec![], None)
        .await;
    let mut conn = layer_connection.codec;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for peer in peers {
        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address: SocketAddress::Ip(addr) })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: addr.into(),
                local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
            },
        ))))
        .await
        .unwrap();

        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite { connection_id: 0, bytes })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes,
            },
        ))))
        .await
        .unwrap();
        conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(0)))
            .await
            .unwrap();
    }

    test_process.wait_assert_success().await;
}

/// Test outgoing TCP.
/// Application, for each remote peer in [`RUST_OUTGOING_PEERS`]:
/// 1. Opens a TCP port at [`RUST_OUTGOING_LOCAL`]
/// 2. Connects to the remote peer
/// 3. Sends some data
/// 4. Expects the peer to send the same data back
async fn outgoing_tcp_logic(with_config: Option<&str>, dylib_path: &PathBuf, config_dir: &PathBuf) {
    let config = with_config.map(|config| {
        let mut config_path = config_dir.clone();
        config_path.push(config);
        config_path
    });
    let config = config.as_ref().map(|path_buf| path_buf.to_str().unwrap());

    let (mut test_process, layer_connection) = Application::RustOutgoingTcp
        .start_process_with_layer(dylib_path, vec![], config)
        .await;
    let mut conn = layer_connection.codec;

    let peers = RUST_OUTGOING_PEERS
        .split(',')
        .map(|s| s.parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    for peer in peers {
        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address: SocketAddress::Ip(addr) })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(addr, peer);
        conn.send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: addr.into(),
                local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
            },
        ))))
        .await
        .unwrap();

        let msg = conn.try_next().await.unwrap().unwrap();
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite { connection_id: 0, bytes })) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        conn.send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes,
            },
        ))))
        .await
        .unwrap();
        conn.send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(0)))
            .await
            .unwrap();
    }

    test_process.wait_assert_success().await;
}

/// See [`outgoing_tcp_logic`].
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn outgoing_tcp(
    #[values(None, Some("outgoing_filter.json"))] with_config: Option<&str>,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    outgoing_tcp_logic(with_config, dylib_path, config_dir).await;
}

/// 1. Tries to go through the [`outgoing_tcp_logic`] flow, except that outgoing traffic is
/// configured to go from the local app, which means that the daemon handler won't be triggered,
/// thus this send will hang (with the whole test hanging) and crashing on timeout, verifying that,
/// indeed, the connection was not relayed to the agent.
///
/// 2. Similar to the [`outgoing_tcp`] test, but we don't add the `remote` address `3.3.3.3` to the
/// list, thus it should go through local, and hang.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
#[should_panic]
async fn outgoing_tcp_from_the_local_app_broken(
    #[values(
        Some("outgoing_filter_local.json"),
        Some("outgoing_filter_remote_incomplete.json")
    )]
    with_config: Option<&str>,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    outgoing_tcp_logic(with_config, dylib_path, config_dir).await;
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(10))]
async fn outgoing_tcp_named_host(
    #[values(Some("outgoing_filter_remote_named_host.json"))] with_config: Option<&str>,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let remote_ip: IpAddr = "6.6.6.6".parse().unwrap();

    let config = with_config.map(|config| {
        let mut config_path = config_dir.clone();
        config_path.push(config);
        config_path
    });
    let config = config.as_ref().map(|path_buf| path_buf.to_str().unwrap());

    let (mut test_process, mut layer_connection) = Application::NodeOutgoingRequest
        .start_process_with_layer(dylib_path, vec![], config)
        .await;

    let resolv_conf = br"
# Generated by NetworkManager
search Home
nameserver 6.6.6.6
nameserver 7.7.7.7";

    let mut fd = 0xb16;
    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::FileRequest(FileRequest::Open(_)) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Open(Ok(
            mirrord_protocol::file::OpenFileResponse { fd },
        ))))
        .await
        .unwrap();
    fd += 1;

    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::FileRequest(FileRequest::Xstat(_)) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Xstat(Ok(
            XstatResponse {
                metadata: MetadataInternal {
                    device_id: 35,
                    inode: 13377877,
                    mode: 33188,
                    hard_links: 1,
                    user_id: 0,
                    group_id: 0,
                    rdevice_id: 0,
                    size: 108,
                    access_time: 124502152,
                    modification_time: 361169354,
                    creation_time: 354507272,
                    block_size: 4096,
                    blocks: 8,
                },
            },
        ))))
        .await
        .unwrap();

    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::FileRequest(FileRequest::Read(_)) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .answer_file_read(resolv_conf.to_vec())
        .await;

    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::FileRequest(FileRequest::Read(_)) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection.answer_file_read(Vec::new()).await;

    let msg = loop {
        let msg = layer_connection.codec.next().await.unwrap().unwrap();
        println!("msg {msg:#?}");

        match msg {
            ClientMessage::FileRequest(FileRequest::Close(_)) => {}
            ClientMessage::FileRequest(FileRequest::Xstat(_)) => {
                layer_connection
                    .codec
                    .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                        XstatResponse {
                            metadata: MetadataInternal {
                                device_id: 35,
                                inode: 13377877,
                                mode: 33188,
                                hard_links: 1,
                                user_id: 0,
                                group_id: 0,
                                rdevice_id: 0,
                                size: 108,
                                access_time: 124502152,
                                modification_time: 361169354,
                                creation_time: 354507272,
                                block_size: 4096,
                                blocks: 8,
                            },
                        },
                    ))))
                    .await
                    .unwrap();
            }
            other => break other,
        };
    };

    let ClientMessage::FileRequest(FileRequest::Open(_)) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .codec
        .send(DaemonMessage::File(FileResponse::Open(Ok(
            mirrord_protocol::file::OpenFileResponse { fd },
        ))))
        .await
        .unwrap();
    fd += 1;

    let msg = loop {
        let msg = layer_connection.codec.next().await.unwrap().unwrap();
        match msg {
            ClientMessage::FileRequest(FileRequest::Xstat(_)) => {
                layer_connection
                    .codec
                    .send(DaemonMessage::File(FileResponse::Xstat(Ok(
                        XstatResponse {
                            metadata: MetadataInternal {
                                device_id: 35,
                                inode: 13377877,
                                mode: 33188,
                                hard_links: 1,
                                user_id: 0,
                                group_id: 0,
                                rdevice_id: 0,
                                size: 108,
                                access_time: 124502152,
                                modification_time: 361169354,
                                creation_time: 354507272,
                                block_size: 4096,
                                blocks: 8,
                            },
                        },
                    ))))
                    .await
                    .unwrap();
            }
            other => break other,
        }
    };

    // let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::FileRequest(FileRequest::Read(_)) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection.answer_file_read(Vec::new()).await;

    // let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest {..}) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .codec
        .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(
            DnsLookup(vec![LookupRecord {
                name: "www.mirrord-magic-service.dev".to_string(),
                ip: remote_ip,
            }]),
        ))))
        .await
        .unwrap();

    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest {..}) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .codec
        .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(
            DnsLookup(vec![LookupRecord {
                name: "www.mirrord-magic-service.dev".to_string(),
                ip: remote_ip,
            }]),
        ))))
        .await
        .unwrap();

    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::TcpOutgoing(
        LayerTcpOutgoing::Connect(
            LayerConnect { remote_address: SocketAddress::Ip(addr) })) = msg else {
        panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .codec
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
            DaemonConnect {
                connection_id: 0,
                remote_address: addr.into(),
                local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
            },
        ))))
        .await
        .unwrap();

    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::TcpOutgoing(
        LayerTcpOutgoing::Write(
            LayerWrite { connection_id: 0, .. })) = msg else {
            panic!("Invalid message {msg:#?}!");
    };
    let minimal_http_response = b"HTTP/1.1 200 OK Content-Length: 0";
    layer_connection
        .codec
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes: minimal_http_response.to_vec(),
            },
        ))))
        .await
        .unwrap();

    let msg = layer_connection.codec.next().await.unwrap().unwrap();
    let ClientMessage::TcpOutgoing(
        LayerTcpOutgoing::Close(
            LayerClose { .. })) = msg else {
            panic!("Invalid message {msg:#?}!");
    };
    layer_connection
        .codec
        .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(0)))
        .await
        .unwrap();

    test_process.wait_assert_success().await;
}

#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{
    outgoing::{
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessage, FileRequest,
};
use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[timeout(Duration::from_secs(60))]
async fn test_dns_resolve(
    #[values(Application::NodeDnsResolve)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let mut conn = layer_connection.codec;
    let mut msg = conn.try_next().await.unwrap().unwrap();

    // FileRequest(Open(OpenFileRequest { path: "/etc/resolv.conf", open_options: OpenOptionsInternal { read: true, write: false, append: false, truncate: false, create: false, create_new: false } }))

    // on linux -> /etc/resolv.conf
    // on macos -> /etc/hostname    
    if let ClientMessage::FileRequest(FileRequest::Open(_)) = msg {
        println!("Message received from layer: {msg:?}");
        msg = conn.try_next().await.unwrap().unwrap();
    };    

    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address: SocketAddress::Ip(addr) })) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };
    conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(Ok(
        DaemonConnect {
            connection_id: 0,
            remote_address: addr.into(),
            local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
        },
    ))))
    .await
    .unwrap();

    msg = conn.try_next().await.unwrap().unwrap();

    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite { connection_id: 0, bytes: _ })) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
        DaemonRead {
            connection_id: 0,
            bytes: vec![
                53, 41, 129, 128, 0, 1, 0, 1, 0, 0, 0, 0, 7, 101, 120, 97, 109, 112, 108, 101, 3,
                99, 111, 109, 0, 0, 1, 0, 1, 7, 101, 120, 97, 109, 112, 108, 101, 3, 99, 111, 109,
                0, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 93, 184, 216, 34,
            ],
        },
    ))))
    .await
    .unwrap();

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}

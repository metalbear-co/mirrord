#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{
    outgoing::{
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessage,
};
use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn recv_from(
    #[values(Application::RustRecvFrom)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let mut conn = layer_connection.codec;
    let msg = conn.try_next().await.unwrap().unwrap();

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

    let msg = conn.try_next().await.unwrap().unwrap();

    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite { connection_id: 0, bytes })) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    // send back the same bytes, the app asserts that they are the same
    conn.send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
        DaemonRead {
            connection_id: 0,
            bytes: bytes,
        },
    ))))
    .await
    .unwrap();

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}

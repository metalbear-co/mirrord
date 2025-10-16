#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    outgoing::{
        DaemonConnect, DaemonRead, LayerWrite,
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
};
use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn recv_from(
    #[values(Application::RustRecvFrom)] application: Application,
    dylib_path: &Path,
) {
    use mirrord_protocol::outgoing::DaemonConnectV2;

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let (uid, addr) = intproxy.recv_udp_connect().await;
    intproxy
        .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::ConnectV2(
            DaemonConnectV2 {
                connect: Ok(DaemonConnect {
                    connection_id: 0,
                    remote_address: addr.into(),
                    local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                }),
                uid,
            },
        )))
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite {
        connection_id: 0,
        bytes,
    })) = msg
    else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    // send back the same bytes, the app asserts that they are the same
    intproxy
        .send(DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(
            DaemonRead {
                connection_id: 0,
                bytes,
            },
        ))))
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

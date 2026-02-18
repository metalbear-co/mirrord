#![cfg(target_family = "unix")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    outgoing::{
        DaemonRead, LayerWrite,
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
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let (uid, addr) = intproxy.recv_udp_connect().await;
    intproxy
        .send_udp_connect_ok(uid, 0, addr, RUST_OUTGOING_LOCAL.parse().unwrap())
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

#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage,
    outgoing::{SocketAddress, v2},
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

    let msg = intproxy.recv().await;
    let ClientMessage::OutgoingV2(v2::ClientOutgoing::Connect(v2::OutgoingConnectRequest {
        id,
        protocol: v2::OutgoingProtocol::Datagrams,
        address: SocketAddress::Ip(addr),
    })) = msg
    else {
        panic!("Invalid message received from layer: {msg:?}");
    };
    intproxy
        .send(
            v2::OutgoingConnectResponse {
                id,
                agent_local_address: RUST_OUTGOING_LOCAL.parse::<SocketAddr>().unwrap().into(),
                agent_peer_address: addr.into(),
            }
            .into(),
        )
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::OutgoingV2(v2::ClientOutgoing::Data(v2::OutgoingData {
        id: received_id,
        data,
    })) = msg
    else {
        panic!("Invalid message received from layer: {msg:?}");
    };
    assert_eq!(received_id, id);

    // send back the same bytes, the app asserts that they are the same
    intproxy.send(v2::OutgoingData { id, data }.into()).await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

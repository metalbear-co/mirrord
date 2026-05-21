#![cfg(target_os = "linux")]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    outgoing::{
        DaemonRead, LayerWrite, SocketAddress, UnixAddr,
        seqpacket::{DaemonSeqpacket, LayerSeqpacket},
    },
};
use rstest::rstest;

mod common;

pub use common::*;

/// Verifies that a Unix `SOCK_SEQPACKET` connection is intercepted by outgoing unix socket
/// handling, routed through intproxy, and can receive a packet back from the fake agent.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn outgoing_unix_seqpacket(config_dir: &Path) {
    let config = config_dir.join("cor_1401_seqpacket.toml");
    let (mut test_process, mut intproxy) = Application::PythonCor1401Seqpacket
        .start_process(vec![], Some(&config))
        .await;

    let expected_address = SocketAddress::Unix(UnixAddr::Pathname(std::path::PathBuf::from(
        COR_1401_SEQPACKET_SOCKET,
    )));

    let (uid, address) = intproxy.recv_seqpacket_connect().await;
    assert_eq!(address, expected_address);

    intproxy
        .send_seqpacket_connect_ok(
            uid,
            0,
            expected_address.clone(),
            SocketAddress::Unix(UnixAddr::Unnamed),
        )
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::SeqpacketOutgoing(LayerSeqpacket::Write(LayerWrite {
        connection_id,
        bytes,
    })) = msg
    else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    assert_eq!(connection_id, 0);

    intproxy
        .send(DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Read(Ok(
            DaemonRead {
                connection_id,
                bytes,
            },
        ))))
        .await;

    intproxy
        .send(DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Close(
            connection_id,
        )))
        .await;

    test_process.wait_assert_success().await;
}

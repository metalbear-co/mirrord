#![cfg(target_family = "unix")]

use std::{path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, ResponseError,
    outgoing::{
        DaemonConnectV2, LayerConnectV2, SocketAddress, UnixAddr,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
    },
};
use rstest::rstest;

mod common;

pub use common::*;

/// Regression test for trailing nulls leaking into outgoing unix socket
/// pathnames. The C app calls `connect(2)` twice with the same `sun_path`
/// but different `addrlen` — first the exact length, then
/// `sizeof(struct sockaddr_un)` so `sun_path` carries trailing null bytes.
/// Without the layer-side trim, the second call's path arrives at the
/// agent with embedded nulls, breaking remote unix connects (and the
/// `unix_streams` regex match too — the second connect would be bypassed
/// to local rather than handed to the intproxy).
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn unix_connect_addrlen_trims_trailing_nulls(
    #[values(Application::UnixConnectAddrlen)] application: Application,
    config_dir: &Path,
) {
    let config_path = config_dir.join("unix_streams.json");
    let (mut test_process, mut intproxy) =
        application.start_process(vec![], Some(&config_path)).await;

    let expected_path = std::path::PathBuf::from("/tmp/mirrord_test_uds_addrlen.sock");

    for label in ["exact", "padded"] {
        let msg = intproxy.recv().await;
        let ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(LayerConnectV2 {
            uid,
            remote_address: SocketAddress::Unix(UnixAddr::Pathname(path)),
        })) = msg
        else {
            panic!("[{label}] unexpected message: {msg:?}");
        };

        assert_eq!(
            path, expected_path,
            "[{label}] layer forwarded a unix socket path with trailing nulls"
        );

        intproxy
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::ConnectV2(
                DaemonConnectV2 {
                    uid,
                    connect: Err(ResponseError::NotImplemented),
                },
            )))
            .await;
    }

    test_process.wait_assert_success().await;
}

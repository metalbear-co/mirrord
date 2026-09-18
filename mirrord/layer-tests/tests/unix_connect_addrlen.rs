#![cfg(target_family = "unix")]

use std::path::Path;

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

/// Regression test for outgoing Unix pathname address lengths. The C app calls
/// `connect(2)` with an exact length, a padded `sockaddr_un`, and PHP's
/// `offsetof(sockaddr_un, sun_path) + strlen(path)` length. All three represent
/// the same pathname to the kernel but socket2 needs the terminating NUL in its
/// logical address length to retain the final path byte.
#[rstest]
#[tokio::test]
async fn unix_connect_addrlen_normalizes_pathname(
    #[values(Application::UnixConnectAddrlen)] application: Application,
    config_dir: &Path,
) {
    let config_path = config_dir.join("unix_streams.json");
    let (mut test_process, mut intproxy) =
        application.start_process(vec![], Some(&config_path)).await;

    let expected_path = std::path::PathBuf::from("/tmp/mirrord_test_uds_addrlen.sock");

    for label in ["exact", "padded", "php-style"] {
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
            "[{label}] layer forwarded an incomplete or null-padded unix socket path"
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

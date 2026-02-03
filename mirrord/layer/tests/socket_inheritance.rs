#![feature(assert_matches)]

mod common;
use std::{
    assert_matches::assert_matches, cell::RefCell, mem::replace, path::Path, time::Duration,
};

pub use common::*;
use mirrord_config::internal_proxy::InternalProxyFileConfig;
use mirrord_protocol::{
    ClientMessage, ConnectionId, DaemonMessage,
    outgoing::{
        DaemonRead, LayerConnectV2, LayerWrite,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
    },
};
use rstest::rstest;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Test that file descriptors are correctly inherited and tracked
/// across fork and exec calls, and are dropped gracefully at the
/// right time.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn socket_inheritance(dylib_path: &Path) {
    let application = Application::SocketInheritance;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let listener_addr = listener.local_addr().unwrap();

    let (mut test_process, intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("TEST_ECHO_SERVER_PORT", &listener_addr.port().to_string())],
            None,
        )
        .await;

    let intproxy = RefCell::new(intproxy);

    let accept_conn = async |id: ConnectionId| {
        let mut intproxy = intproxy.borrow_mut();
        let (uid, _) = intproxy.recv_tcp_connect().await;
        intproxy
            .send_tcp_connect_ok(uid, id, listener_addr, "127.0.0.1:10000".parse().unwrap())
            .await;
    };

    let pong = async |id: ConnectionId| {
        let mut intproxy = intproxy.borrow_mut();

        assert_matches!(intproxy.recv().await,
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes,
            })) if connection_id == id && &**bytes == b"PING");

        intproxy
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: id,
                    bytes: "PONG".into(),
                },
            ))))
            .await;
    };

    let graceful_shutdown = async |id: ConnectionId| {
        let mut intproxy = intproxy.borrow_mut();
        assert_matches!(
            intproxy.recv().await,
            ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes
            })) if connection_id == id && bytes.is_empty()
        );

        intproxy
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: id,
                    bytes: vec![].into(),
                },
            ))))
            .await;

        // TODO(areg): Expect a close from the agent here once that
        // change is merged merged
    };

    accept_conn(0).await;
    accept_conn(1).await;

    // Before fork
    pong(0).await;
    pong(1).await;

    // After fork (in the child, both are alive)
    pong(0).await;
    pong(1).await;

    // The first connection should now close down, as it was CLOEXEC
    // so the child doesn't have it, and the parent has closed it.
    graceful_shutdown(0).await;

    // Final one from the child
    pong(1).await;
    graceful_shutdown(1).await;

    assert_eq!(intproxy.borrow_mut().try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

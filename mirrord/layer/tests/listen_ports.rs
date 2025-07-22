#![cfg(target_os = "linux")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{
    assert_matches::assert_matches,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};

use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcpSteal, StealType},
    ClientMessage, DaemonMessage,
};
use rstest::rstest;
use tokio::{io::AsyncWriteExt, net::TcpListener};

mod common;

pub use common::*;
use tokio::net::TcpStream;

/// Verifies that the layer respects `feature.network.incoming.listen_ports` mapping.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn listen_ports(
    #[values(Application::RustListenPorts)] application: Application,
    dylib_path: &Path,
) {
    // We need to know ports on which the application listens,
    // because we want to make the connections from the test code
    // (to verify that the layer respects `feature.network.incoming.listen_ports` mapping).
    // If we just use const ports, this test is flaky in the CI, as the app tends to fail with
    // EADDRINUSE. Here we bind to random ports, drop the sockets, and hope that the ports
    // remain free for the app to use.
    let port_1 = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let port_2 = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let config = serde_json::json!({
        "feature": {
            "network": {
                "incoming": {
                    "mode": "steal",
                    "listen_ports": [[80, port_1]]
                }
            },
            "fs": "local"
        }
    });
    // Ports for the application code to use.
    // Due to the port mapping, the application should effectively listen on `port_1;port_2`.
    let app_ports = format!("80;{port_2}");

    let mut config_file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    config_file
        .as_file_mut()
        .write_all(serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("RUST_LOG", "mirrord=trace"), ("APP_PORTS", &app_ports)],
            Some(config_file.path()),
        )
        .await;

    for (subscription_port, local_port) in [(80, port_1), (port_2, port_2)] {
        assert_matches!(
            intproxy.recv().await,
            ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(port)))
            if port == subscription_port,
        );
        intproxy
            .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
                subscription_port,
            ))))
            .await;
        let mut stream =
            TcpStream::connect(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), local_port))
                .await
                .unwrap();
        println!("connected to the application on port {local_port} (application code used {subscription_port})");
        stream.write_all(b"HELLO").await.unwrap();
        stream.shutdown().await.unwrap();
        assert_matches!(
            intproxy.recv().await,
            ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(port))
            if port == subscription_port,
        );
    }

    assert_eq!(intproxy.try_recv().await, None);

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

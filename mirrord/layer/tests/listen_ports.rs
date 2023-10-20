#![cfg(target_os = "linux")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{assert_matches::assert_matches, path::PathBuf, time::Duration};

use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcpSteal, StealType},
    ClientMessage, DaemonMessage,
};
use rstest::rstest;
use tokio::io::AsyncWriteExt;

mod common;

pub use common::*;
use tokio::net::TcpStream;

/// Start an application (and load the layer into it) that listens on a port that is configured to
/// be ignored, and verify that no messages are sent to the agent.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn listen_ports(
    #[values(Application::RustListenPorts)] application: Application,
    dylib_path: &PathBuf,
    config_dir: &PathBuf,
) {
    let mut config_path = config_dir.clone();
    config_path.push("listen_ports.json");
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![
                ("RUST_LOG", "mirrord=trace"),
                ("MIRRORD_FILE_MODE", "local"),
            ],
            Some(config_path.to_str().unwrap()),
        )
        .await;

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(80)))
    );
    intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(80))))
        .await;
    let mut stream = TcpStream::connect("127.0.0.1:51222").await.unwrap();
    println!("connected to listener at port 51222");
    stream.write_all(b"HELLO").await.unwrap();

    assert_matches!(
        intproxy.recv().await,
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(40000)))
    );
    intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
            40000,
        ))))
        .await;
    let mut stream = TcpStream::connect("127.0.0.1:40000").await.unwrap();
    println!("connected to listener at port 40000");
    stream.write_all(b"HELLO").await.unwrap();

    loop {
        match intproxy.try_recv().await {
            Some(ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(40000))) => {}
            Some(ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(80))) => {}
            None => break,
            other => panic!("unexpected message: {:?}", other),
        }
    }
    assert_eq!(intproxy.try_recv().await, None);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

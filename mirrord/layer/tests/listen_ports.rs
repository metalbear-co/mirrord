#![cfg(target_os = "linux")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{assert_matches::assert_matches, path::PathBuf, time::Duration};

use futures::StreamExt;
use mirrord_protocol::{
    tcp::{LayerTcpSteal, StealType},
    ClientMessage,
};
use rstest::rstest;

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
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(
            dylib_path,
            vec![("MIRRORD_FILE_MODE", "local")],
            Some(config_path.to_str().unwrap()),
        )
        .await;

    assert_matches!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(80)))
    );

    TcpStream::connect("127.0.0.1:51222").await.unwrap();

    assert_matches!(
        layer_connection.codec.next().await.unwrap().unwrap(),
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(40000)))
    );

    TcpStream::connect("127.0.0.1:40000").await.unwrap();

    assert!(layer_connection.is_ended().await);
    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr();
    test_process.assert_no_error_in_stdout();
}

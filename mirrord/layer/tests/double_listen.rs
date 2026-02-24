#![cfg(target_family = "unix")]
#![feature(assert_matches)]

use rstest::rstest;

mod common;

use std::{io::Write, path::Path, time::Duration};

pub use common::*;
use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
    file::OpenFileResponse,
    tcp::{DaemonTcp, LayerTcpSteal, StealType},
};

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(15))]
async fn double_listen(dylib_path: &Path) {
    let config = serde_json::json!({
        "target": "pod/real-pod",
        "feature": {
            "network": {
                "incoming": {
                    "mode": "steal",
                    "ports": [4567]
                }
            }
        }
    });
    let mut config_file = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    config_file
        .as_file_mut()
        .write_all(serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();

    let (mut test_process, mut intproxy) = Application::DoubleListen
        .start_process_with_layer(dylib_path, vec![], Some(config_file.path()))
        .await;

    let ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(port))) =
        intproxy.recv().await
    else {
        panic!("no port subscribe request")
    };
    assert_eq!(port, 4567);

    intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
            port,
        ))))
        .await;

    let file_request = match intproxy.recv().await {
        // Port unsubscribed after calling listen the second time
        ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(4567)) => {
            panic!("listener socket closed")
        }
        ClientMessage::FileRequest(FileRequest::Open(req)) => req,
        _ => panic!("unexpected client message"),
    };
    assert_eq!(file_request.path.to_str().unwrap(), "/why_double_listen");

    intproxy
        .send(DaemonMessage::File(FileResponse::Open(Ok(
            OpenFileResponse { fd: 1 },
        ))))
        .await;

    test_process.wait_assert_success().await;
}

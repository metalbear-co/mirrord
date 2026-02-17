#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64",)
))]
#![feature(assert_matches)]

use rstest::rstest;

mod common;

use std::{path::Path, time::Duration};

pub use common::*;
use mirrord_protocol::tcp::{LayerTcpSteal, StealType};

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_dlopen_cgo(
    #[values(Application::DlopenCgoCShared)] application: Application,
    dylib_path: &Path,
) {
    use mirrord_protocol::ClientMessage;

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("dlopen_cgo.json");
    let config = serde_json::json!({
        "feature": {
            "network": {
                "incoming": {
                    "mode": "steal"
                }
            }
        },
        "experimental": {
            "dlopen_cgo": true
        }
    });
    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .await
        .expect("failed to saving layer config to tmp file");

    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![("RUST_LOG", "mirrord=warn")],
            Some(&config_path),
        )
        .await;

    let message = intproxy.recv().await;
    let steal_type = match message {
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(steal_type)) => steal_type,
        other => panic!("unexpected message received from the app: {other:?}"),
    };
    assert_eq!(steal_type, StealType::All(23333));

    test_process
        .child
        .kill()
        .await
        .expect("failed to kill the app");
}

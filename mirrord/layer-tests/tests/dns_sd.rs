#![cfg(target_os = "macos")]

use std::{net::IpAddr, path::Path};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, DnsLookupError,
    ResolveErrorKindInternal::NoRecordsFound,
    ResponseError,
    dns::{DnsLookup, GetAddrInfoRequestV2, GetAddrInfoResponse, LookupRecord},
};
use rstest::rstest;

mod common;
pub use common::*;

/// Verify that macOS DNS-SD address queries (what Bun uses) are resolved through the agent, for
/// every DNS-SD client style, and that the queries we don't answer go to the daemon.
///
/// The app checks what each query receives. Here we check that only the expected names reach the
/// agent, each exactly once: queries we don't answer, and queries deallocated before their reply
/// is processed, never do.
#[rstest]
#[tokio::test]
async fn dns_sd(config_dir: &Path) {
    let config_path = config_dir.join("dns_sd.json");
    let (mut test_process, mut intproxy) = Application::DnsSd
        .start_process(vec![("MIRRORD_REMOTE_DNS", "true")], Some(&config_path))
        .await;

    let lookups: [(&str, &[&str]); 5] = [
        ("remote.test", &["10.0.0.1"]),
        ("ex.test", &["10.0.0.2"]),
        ("standalone.test", &["10.0.0.3", "fd00::3"]),
        ("dispatch.test", &["10.0.0.4"]),
        ("missing.test", &[]),
    ];

    for (expected_node, addresses) in lookups {
        let msg = intproxy.recv().await;
        let ClientMessage::GetAddrInfoRequestV2(GetAddrInfoRequestV2 { node, .. }) = msg else {
            panic!("Invalid message received from layer: {msg:?}");
        };
        assert_eq!(node, expected_node);

        let response = if addresses.is_empty() {
            Err(ResponseError::DnsLookup(DnsLookupError {
                kind: NoRecordsFound(3),
            }))
        } else {
            Ok(DnsLookup(
                addresses
                    .iter()
                    .map(|address| LookupRecord {
                        name: node.clone(),
                        ip: address.parse::<IpAddr>().unwrap(),
                    })
                    .collect(),
            ))
        };

        intproxy
            .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(
                response,
            )))
            .await;
    }

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test dns_sd: SUCCESS")
        .await;
}

#![feature(assert_matches)]
use std::{net::IpAddr, path::PathBuf, time::Duration};

use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    ClientMessage, DaemonMessageV1, DnsLookupError,
    ResolveErrorKindInternal::NoRecordsFound,
    ResponseError,
};
use rstest::rstest;

mod common;
pub use common::*;

/// Verify that issue [#2055](https://github.com/metalbear-co/mirrord/issues/2055) is fixed.
/// "DNS Issue on Elixir macOS"
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn issue_2055(dylib_path: &PathBuf) {
    let application = Application::CIssue2055;
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, Default::default(), None)
        .await;

    println!("Application started, waiting for `GetAddrInfoRequest`.");

    let msg = intproxy.recv().await;
    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    intproxy
        .send(DaemonMessageV1::GetAddrInfoResponse(GetAddrInfoResponse(
            Ok(DnsLookup(vec![LookupRecord {
                name: node,
                ip: "93.184.216.34".parse::<IpAddr>().unwrap(),
            }])),
        )))
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node: _ }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    intproxy
        .send(DaemonMessageV1::GetAddrInfoResponse(GetAddrInfoResponse(
            Err(ResponseError::DnsLookup(DnsLookupError {
                kind: NoRecordsFound(3),
            })),
        )))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("test issue 2055: START")
        .await;
    test_process
        .assert_stdout_contains("test issue 2055: SUCCESS")
        .await;
}

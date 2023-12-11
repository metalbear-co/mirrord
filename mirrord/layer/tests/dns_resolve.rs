#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration, vec};

use rstest::rstest;

mod common;
pub use common::*;
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    ClientMessage, DaemonMessageV1, DnsLookupError,
    ResolveErrorKindInternal::NoRecordsFound,
};

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_dns_resolve(
    #[values(Application::RustDnsResolve)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_REMOTE_DNS", "true")], None)
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    intproxy
        .send(DaemonMessageV1::GetAddrInfoResponse(GetAddrInfoResponse(
            Ok(DnsLookup(vec![LookupRecord {
                name: node,
                ip: "93.184.216.34".parse::<std::net::IpAddr>().unwrap(),
            }])),
        )))
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node: _ }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    intproxy
        .send(DaemonMessageV1::GetAddrInfoResponse(GetAddrInfoResponse(
            Err(mirrord_protocol::ResponseError::DnsLookup(DnsLookupError {
                kind: NoRecordsFound(3),
            })),
        )))
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

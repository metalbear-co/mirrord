#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::Path, time::Duration, vec};

use rstest::rstest;

mod common;
pub use common::*;
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequestV2, GetAddrInfoResponse, LookupRecord},
    ClientMessage, DaemonMessage, DnsLookupError,
    ResolveErrorKindInternal::NoRecordsFound,
};

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_dns_resolve(
    #[values(Application::RustDnsResolve)] application: Application,
    dylib_path: &Path,
) {
    let (mut test_process, mut intproxy) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_REMOTE_DNS", "true")], None)
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::GetAddrInfoRequestV2(GetAddrInfoRequestV2 { node, .. }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    intproxy
        .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(
            DnsLookup(vec![LookupRecord {
                name: node,
                ip: "93.184.216.34".parse::<std::net::IpAddr>().unwrap(),
            }]),
        ))))
        .await;

    let msg = intproxy.recv().await;
    let ClientMessage::GetAddrInfoRequestV2(GetAddrInfoRequestV2 { .. }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    intproxy
        .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(
            Err(mirrord_protocol::ResponseError::DnsLookup(DnsLookupError {
                kind: NoRecordsFound(3),
            })),
        )))
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

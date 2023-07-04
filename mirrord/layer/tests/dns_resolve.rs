#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{path::PathBuf, time::Duration, vec};

use futures::TryStreamExt;
use rstest::rstest;

mod common;
pub use common::*;
use futures::SinkExt;
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    ClientMessage, DaemonMessage, DnsLookupError,
    ResolveErrorKindInternal::NoRecordsFound,
};

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_dns_resolve(
    #[values(Application::RustDnsResolve)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, layer_connection) = application
        .start_process_with_layer(dylib_path, vec![("MIRRORD_REMOTE_DNS", "true")], None)
        .await;

    let mut conn = layer_connection.codec;
    let msg = conn.try_next().await.unwrap().unwrap();

    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    conn.send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(
        DnsLookup(vec![LookupRecord {
            name: node,
            ip: "93.184.216.34".parse::<std::net::IpAddr>().unwrap(),
        }]),
    ))))
    .await
    .unwrap();

    let msg = conn.try_next().await.unwrap().unwrap();

    let ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest { node: _ }) = msg else {
        panic!("Invalid message received from layer: {msg:?}");
    };

    conn.send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(
        Err(mirrord_protocol::ResponseError::DnsLookup(DnsLookupError {
            kind: NoRecordsFound(3),
        })),
    )))
    .await
    .unwrap();

    test_process.wait_assert_success().await;
    test_process.assert_no_error_in_stderr().await;
    test_process.assert_no_error_in_stdout().await;
}

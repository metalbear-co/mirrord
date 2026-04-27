#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::{io::Write, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    dns::{DnsLookup, GetAddrInfoRequestV2, GetAddrInfoResponse},
};
use rstest::rstest;
use tempfile::NamedTempFile;

mod common;

pub use common::*;

/// Verify that Node gets a regular lookup error instead of aborting when remote DNS returns no
/// records.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_node_getaddrinfo_no_name_returns_error() {
    let mut app = NamedTempFile::with_suffix(".js").unwrap();
    app.as_file_mut()
        .write_all(
            br#"const dns = require("dns");

dns.lookup("missing.example.test", (err) => {
    if (!err) {
        console.error("lookup unexpectedly succeeded");
        process.exit(1);
    }

    console.log(`lookup failed with ${err.code ?? "UNKNOWN"}`);
    process.exit(0);
});
"#,
        )
        .unwrap();

    let application = Application::DynamicApp(
        "node".to_string(),
        vec![app.path().to_string_lossy().to_string()],
    );

    let (mut test_process, mut intproxy) = application
        .start_process(vec![("MIRRORD_REMOTE_DNS", "true")], None)
        .await;

    if cfg!(target_os = "macos") {
        intproxy
            .expect_file_open_for_reading("/etc/resolv.conf", 2136)
            .await;
        intproxy
            .consume_xstats_then_expect_file_read("search home\nnameserver 10.0.0.138\n", 2136)
            .await;
        intproxy.expect_file_close(2136).await;
    }

    let message = intproxy.recv().await;
    let ClientMessage::GetAddrInfoRequestV2(GetAddrInfoRequestV2 { node, .. }) = message else {
        panic!("Invalid message received from layer: {message:?}");
    };
    assert_eq!(node, "missing.example.test");

    intproxy
        .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(
            DnsLookup(vec![]),
        ))))
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("lookup failed with")
        .await;
    test_process.assert_no_error_in_stderr().await;
}

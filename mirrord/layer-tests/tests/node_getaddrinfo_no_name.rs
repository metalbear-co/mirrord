#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::{io::Write, path::PathBuf, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
    dns::{DnsLookup, GetAddrInfoRequestV2, GetAddrInfoResponse},
    file::{
        OpenFileRequest, OpenFileResponse, OpenOptionsInternal, SeekFileRequest, SeekFileResponse,
        SeekFromInternal,
    },
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
    const RESOLV_CONF_CONTENTS: &str = "search home\nnameserver 10.0.0.138\n";
    const RESOLV_CONF_FD: u64 = 2136;

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

    let GetAddrInfoRequestV2 { node, .. } = loop {
        let message = intproxy.consume_xstats().await;

        match message {
            ClientMessage::GetAddrInfoRequestV2(request) => break request,
            ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest {
                path,
                open_options:
                    OpenOptionsInternal {
                        read: true,
                        write: false,
                        append: false,
                        truncate: false,
                        create: false,
                        create_new: false,
                    },
            })) => {
                assert_eq!(path, PathBuf::from("/etc/resolv.conf"));

                intproxy
                    .send(DaemonMessage::File(FileResponse::Open(Ok(
                        OpenFileResponse { fd: RESOLV_CONF_FD },
                    ))))
                    .await;

                match intproxy.consume_xstats().await {
                    ClientMessage::FileRequest(FileRequest::Seek(SeekFileRequest {
                        fd,
                        seek_from: SeekFromInternal::Start(0),
                    })) => {
                        assert_eq!(fd, RESOLV_CONF_FD);

                        intproxy
                            .send(DaemonMessage::File(FileResponse::Seek(Ok(
                                SeekFileResponse { result_offset: 0 },
                            ))))
                            .await;
                        intproxy
                            .consume_xstats_then_expect_file_read(
                                RESOLV_CONF_CONTENTS,
                                RESOLV_CONF_FD,
                            )
                            .await;
                    }
                    message => {
                        let buffer_size =
                            TestIntProxy::expect_message_file_read(message, RESOLV_CONF_FD).await;
                        intproxy
                            .answer_file_read_twice(
                                RESOLV_CONF_CONTENTS,
                                RESOLV_CONF_FD,
                                buffer_size,
                            )
                            .await;
                    }
                }

                intproxy.expect_file_close(RESOLV_CONF_FD).await;
            }
            other => panic!("Invalid message received from layer: {other:?}"),
        }
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

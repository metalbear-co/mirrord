#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::{io::Write, path::Path, time::Duration};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
    dns::{DnsLookup, GetAddrInfoRequestV2, GetAddrInfoResponse},
    file::{
        CloseFileRequest, OpenFileRequest, OpenFileResponse, OpenOptionsInternal, ReadFileRequest,
        SeekFileRequest, SeekFileResponse, SeekFromInternal,
    },
};
use rstest::rstest;
use tempfile::NamedTempFile;

mod common;

pub use common::*;

async fn serve_remote_file(intproxy: &mut TestIntProxy, contents: &str, remote_fd: u64) {
    intproxy
        .send(DaemonMessage::File(FileResponse::Open(Ok(
            OpenFileResponse { fd: remote_fd },
        ))))
        .await;

    let mut cursor = 0usize;

    loop {
        match intproxy.consume_xstats().await {
            ClientMessage::FileRequest(FileRequest::Seek(SeekFileRequest {
                fd,
                seek_from: SeekFromInternal::Start(0),
            })) => {
                assert_eq!(fd, remote_fd);
                cursor = 0;

                intproxy
                    .send(DaemonMessage::File(FileResponse::Seek(Ok(
                        SeekFileResponse { result_offset: 0 },
                    ))))
                    .await;
            }
            ClientMessage::FileRequest(FileRequest::Read(ReadFileRequest {
                remote_fd: requested_fd,
                buffer_size,
            })) => {
                assert_eq!(requested_fd, remote_fd);

                let end = cursor
                    .saturating_add(buffer_size as usize)
                    .min(contents.len());
                let bytes = contents
                    .as_bytes()
                    .get(cursor..end)
                    .unwrap_or_default()
                    .to_vec();
                cursor = end;

                intproxy.answer_file_read(bytes).await;
            }
            ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd })) => {
                assert_eq!(fd, remote_fd);
                break;
            }
            other => panic!("Invalid message while serving remote file: {other:?}"),
        }
    }
}

/// Verify that Node gets a regular lookup error instead of aborting when remote DNS returns no
/// records.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn test_node_getaddrinfo_no_name_returns_error() {
    const HOSTNAME_CONTENTS: &str = "github-actions\n";
    const HOSTS_CONTENTS: &str = "127.0.0.1 localhost\n::1 localhost\n";
    const NSSWITCH_CONTENTS: &str = "hosts: files dns\n";
    const RESOLV_CONF_CONTENTS: &str = "search home\nnameserver 10.0.0.138\n";
    const REMOTE_FILE_FD: u64 = 2136;

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
                let contents = match path.as_path() {
                    path if path == Path::new("/etc/hostname") => HOSTNAME_CONTENTS,
                    path if path == Path::new("/etc/hosts") => HOSTS_CONTENTS,
                    path if path == Path::new("/etc/nsswitch.conf") => NSSWITCH_CONTENTS,
                    path if path == Path::new("/etc/resolv.conf") => RESOLV_CONF_CONTENTS,
                    _ => panic!(
                        "Unexpected file opened before getaddrinfo: {}",
                        path.display()
                    ),
                };

                serve_remote_file(&mut intproxy, contents, REMOTE_FILE_FD).await;
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

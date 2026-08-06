#![cfg(target_family = "unix")]
#![warn(clippy::indexing_slicing)]

use std::io::Write;

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
    dns::{DnsLookup, GetAddrInfoRequestV2, GetAddrInfoResponse, LookupRecord},
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

/// Verify that the layer survives in a process that was `exec`'d with stdin closed.
///
/// Without the std fd guard, the layer's connection to the internal proxy is assigned fd 0,
/// `libuv` treats it as stdin and sets `O_NONBLOCK` on it once the app touches
/// `process.stdin`, and the layer's next blocking read fails with `EAGAIN`, killing the app.
/// This broke Next.js with Turbopack, which spawns its workers with stdin closed.
/// See [#4622](https://github.com/metalbear-co/mirrord/issues/4622).
#[rstest]
#[tokio::test]
async fn issue4622() {
    const REMOTE_FILE_FD: u64 = 4622;

    let config = serde_json::json!({
        "experimental": {
            "guard_std_fds": true,
        },
        "feature": {
            "network": {
                "incoming": false,
                "dns": true,
            },
            "fs": "local",
            "hostname": false,
        }
    });
    let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
    config_file
        .as_file_mut()
        .write_all(serde_json::to_string_pretty(&config).unwrap().as_bytes())
        .unwrap();

    let (mut test_process, mut intproxy) = Application::NodeIssue4622
        .start_process(
            vec![("MIRRORD_REMOTE_DNS", "true")],
            Some(config_file.path()),
        )
        .await;

    let GetAddrInfoRequestV2 { node, .. } = loop {
        let message = intproxy.consume_xstats().await;

        match message {
            ClientMessage::GetAddrInfoRequestV2(request) => break request,
            ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest {
                open_options:
                    OpenOptionsInternal {
                        read: true,
                        write: false,
                        append: false,
                        truncate: false,
                        create: false,
                        create_new: false,
                    },
                ..
            })) => {
                // Node probes host-related config files (`/etc/resolv.conf`, `/etc/hosts`, ...)
                // before DNS resolution. Their contents don't matter here; the test only cares
                // about the layer surviving until the lookup completes.
                serve_remote_file(&mut intproxy, "", REMOTE_FILE_FD).await;
            }
            other => panic!("Invalid message received from layer: {other:?}"),
        }
    };
    assert_eq!(node, "example.com");

    intproxy
        .send(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(
            DnsLookup(vec![LookupRecord {
                name: node,
                ip: "93.184.216.34".parse().unwrap(),
            }]),
        ))))
        .await;

    test_process.wait_assert_success().await;
    test_process.assert_stdout_contains("layer survived").await;
}

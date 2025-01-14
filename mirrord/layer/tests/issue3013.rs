#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use hyper::{
    header::{HeaderName, HeaderValue},
    Method, StatusCode, Version,
};
use mirrord_protocol::{
    self,
    file::{
        CloseFileRequest, OpenFileRequest, OpenFileResponse, ReadFileRequest, ReadFileResponse,
    },
    tcp::{
        DaemonTcp, HttpRequest, HttpResponse, InternalHttpRequest, InternalHttpResponse,
        LayerTcpSteal, StealType,
    },
    ClientMessage, DaemonMessage, FileRequest, FileResponse,
};
use rstest::rstest;

mod common;

pub use common::*;

/// Verifies that [issue 3013](https://github.com/metalbear-co/mirrord/issues/3013) is resolved.
///
/// The issue was that the first request was leaving behind a lingering HTTP connection, that was in
/// turn blocking the local application. The lingering connection was not a bug on our side, but
/// still we can handle this case more smoothly.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(60))]
async fn issue_3013(dylib_path: &Path) {
    let config_file = tempfile::tempdir().unwrap();
    let config = serde_json::json!(
        {
            "feature": {
                "network": {
                    "outgoing": false,
                    "dns": false,
                    "incoming": {
                        "mode": "steal",
                        "http_filter": {
                            "header_filter": "x-filter: yes",
                        },
                    }
                },
                "fs": {
                    "mode": "local",
                }
            },
        }
    );
    let config_path = config_file.path().join("config.json");
    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .await
        .unwrap();

    let (test_process, mut test_intproxy) = Application::PythonHTTPChunked
        .start_process_with_layer(dylib_path, vec![], Some(&config_path.to_string_lossy()))
        .await;

    match test_intproxy.recv().await {
        ClientMessage::FileRequest(FileRequest::Open(OpenFileRequest { path, .. })) => {
            assert_eq!(path, PathBuf::from("/etc/hostname"));
        }
        other => panic!("unexpected message from intproxy: {other:?}"),
    }
    test_intproxy
        .send(DaemonMessage::File(FileResponse::Open(Ok(
            OpenFileResponse { fd: 2137 },
        ))))
        .await;
    match test_intproxy.recv().await {
        ClientMessage::FileRequest(FileRequest::Read(ReadFileRequest {
            remote_fd: 2137, ..
        })) => {}
        other => panic!("unexpected message from intproxy: {other:?}"),
    }
    test_intproxy
        .send(DaemonMessage::File(FileResponse::Read(Ok(
            ReadFileResponse {
                bytes: "test-hostname".as_bytes().into(),
                read_amount: 13,
            },
        ))))
        .await;
    match test_intproxy.recv().await {
        ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd: 2137 })) => {}
        other => panic!("unexpected message from intproxy: {other:?}"),
    }

    match test_intproxy.recv().await {
        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::FilteredHttpEx(..))) => {}
        other => panic!("unexpected message from intproxy: {other:?}"),
    }
    test_intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(80))))
        .await;
    test_process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;
    println!("The application subscribed the port");

    println!("Sending the first request to the intproxy");
    test_intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::HttpRequestFramed(
            HttpRequest {
                internal_request: InternalHttpRequest {
                    method: Method::GET,
                    uri: "/some/path".parse().unwrap(),
                    headers: [(
                        HeaderName::from_static("connection"),
                        HeaderValue::from_static("keep-alive"),
                    )]
                    .into_iter()
                    .collect(),
                    version: Version::HTTP_11,
                    body: Default::default(),
                },
                connection_id: 0,
                request_id: 0,
                port: 80,
            },
        )))
        .await;
    match test_intproxy.recv().await {
        ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(HttpResponse {
            port: 80,
            connection_id: 0,
            request_id: 0,
            internal_response:
                InternalHttpResponse {
                    status: StatusCode::OK,
                    version: Version::HTTP_11,
                    ..
                },
        })) => {}
        other => panic!("unexpected message from intproxy: {other:?}"),
    }
    println!("Received the first response from the intproxy");

    println!("Sending the second request to the intproxy, without closing the first connection");
    test_intproxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::HttpRequestFramed(
            HttpRequest {
                internal_request: InternalHttpRequest {
                    method: Method::GET,
                    uri: "/some/path".parse().unwrap(),
                    headers: [(
                        HeaderName::from_static("connection"),
                        HeaderValue::from_static("keep-alive"),
                    )]
                    .into_iter()
                    .collect(),
                    version: Version::HTTP_11,
                    body: Default::default(),
                },
                connection_id: 1,
                request_id: 0,
                port: 80,
            },
        )))
        .await;
    match test_intproxy.recv().await {
        ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(HttpResponse {
            port: 80,
            connection_id: 1,
            request_id: 0,
            internal_response:
                InternalHttpResponse {
                    status: StatusCode::OK,
                    version: Version::HTTP_11,
                    ..
                },
        })) => {}
        other => panic!("unexpected message from intproxy: {other:?}"),
    }
    println!("Received the second response from the intproxy");
}

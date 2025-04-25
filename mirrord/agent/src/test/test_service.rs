#![cfg(test)]

use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    http::StatusCode,
    upgrade::Upgraded,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::http::HttpVersion;

pub fn full_body<B: AsRef<[u8]>>(body: B) -> BoxBody<Bytes, hyper::Error> {
    Full::<Bytes>::new(body.as_ref().to_vec().into())
        .map_err(|_| unreachable!())
        .boxed()
}

pub struct TestRequest {
    pub request: Request<Incoming>,
    pub response_tx: oneshot::Sender<Response<BoxBody<Bytes, hyper::Error>>>,
}

impl TestRequest {
    pub fn assert_header(&self, header: &str, value: Option<&str>) {
        let found = self.request.headers().get(header);

        match value {
            Some(expected_value) => {
                let found_value = found
                    .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
                    .unwrap_or_else(|| panic!("header {header} is missing or invalid: {found:?}"));
                assert_eq!(
                    found_value, expected_value,
                    "unexpected value of header {header}"
                );
            }
            None => {
                assert_eq!(found, None, "header {header} should not be present");
            }
        }
    }

    pub async fn assert_body(&mut self, body: &[u8]) {
        let mut collected = vec![];

        while let Some(frame) = self.request.body_mut().frame().await {
            let frame = frame.unwrap().into_data().unwrap();
            collected.extend_from_slice(&frame);
        }

        assert_eq!(
            collected,
            body,
            "found body `{}`, expected `{}`",
            String::from_utf8_lossy(&collected),
            String::from_utf8_lossy(body),
        );
    }

    pub async fn assert_ready_body(&mut self, body: &[u8]) {
        let mut collected = vec![];

        while collected != body {
            let frame = self
                .request
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
            collected.extend_from_slice(&frame);
            assert!(
                body.starts_with(&collected),
                "found body `{}`, expected `{}`",
                String::from_utf8_lossy(&collected),
                String::from_utf8_lossy(body),
            );
        }
    }

    pub fn respond_simple(self, status: StatusCode, body: &[u8]) {
        let response = Response::builder()
            .version(self.request.version())
            .status(status)
            .body(full_body(body))
            .unwrap();

        self.response_tx.send(response).unwrap();
    }

    pub fn respond_streaming(self, status: StatusCode) -> mpsc::Sender<Vec<u8>> {
        let (body_tx, body_rx) = mpsc::channel(8);
        let stream = ReceiverStream::new(body_rx).map(|data| Ok(Frame::data(Bytes::from(data))));

        let response = Response::builder()
            .version(self.request.version())
            .status(status)
            .body(BoxBody::new(StreamBody::new(stream)))
            .unwrap();

        self.response_tx.send(response).unwrap();

        body_tx
    }

    pub async fn respond_expect_upgrade(mut self, protocol_name: &str) -> TokioIo<Upgraded> {
        let upgraded = hyper::upgrade::on(&mut self.request);

        let response = Response::builder()
            .version(self.request.version())
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("connection", "upgrade")
            .header("upgrade", protocol_name)
            .body(full_body([]))
            .unwrap();

        self.response_tx.send(response).unwrap();
        TokioIo::new(upgraded.await.unwrap())
    }
}

pub async fn run_test_service(
    listener: TcpListener,
    version: HttpVersion,
    request_tx: mpsc::Sender<TestRequest>,
    shutdown: CancellationToken,
) {
    let mut tasks = JoinSet::new();

    loop {
        let result = tokio::select! {
            _ = shutdown.cancelled() => {
                break;
            }
            result = listener.accept() => result,
        };

        let (stream, peer) = result.unwrap();
        println!("Accepted connection from {peer}");

        let shutdown = shutdown.clone().cancelled_owned();
        let request_tx = request_tx.clone();
        let service = hyper::service::service_fn(move |request| {
            println!("Received request from {peer}: {request:?}");
            let request_tx = request_tx.clone();
            async move {
                let (response_tx, response_rx) = oneshot::channel();
                request_tx
                    .send(TestRequest {
                        request,
                        response_tx,
                    })
                    .await
                    .unwrap();
                let response = response_rx.await.unwrap();
                println!("Sending response to {peer}: {response:?}");
                Ok(response)
            }
            .boxed()
        });

        tasks.spawn(async move {
            crate::http::run_http_server(stream, service, version, shutdown)
                .await
                .unwrap();
        });
    }

    tasks.join_all().await;
}

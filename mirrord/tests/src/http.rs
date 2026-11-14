#![cfg(test)]

use std::{cmp::Ordering, time::Duration};

use http_body_util::BodyExt;
use hyper::{client::conn::http1::SendRequest, Method, Request};
use hyper_util::rt::TokioIo;
use rstest::*;
use tokio::net::TcpStream;

use crate::utils::{
    application::Application, client::kube_client, images::HTTP_KEEP_ALIVE_IMAGE,
    port_forwarder::PortForwarder, services::basic_service, KubeClient,
};

async fn make_http_conn(portforwarder: &PortForwarder) -> SendRequest<String> {
    let tcp_conn = TcpStream::connect(portforwarder.address())
        .await
        .expect("failed to make a TCP connection to the remote app");
    let (sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, String>(TokioIo::new(tcp_conn))
        .await
        .expect("failed to make an HTTP/1 connection to the remote app");
    tokio::spawn(conn);
    sender
}

async fn send_and_verify(
    sender: &mut SendRequest<String>,
    request: Request<String>,
    expected_response_body: &str,
) {
    println!("Sending request");
    sender
        .ready()
        .await
        .expect("failed to wait for the HTTP connection to be ready");
    let response = sender
        .send_request(request)
        .await
        .expect("failed to make an HTTP request to the remote app");
    let (parts, body) = response.into_parts();
    let body_result = body
        .collect()
        .await
        .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned());
    println!(
        "Received response {}. BODY={:?}, HEADERS={:?}",
        parts.status, body_result, parts.headers,
    );
    let body = body_result.expect("failed to read body of the HTTP response from the remote app");
    assert_eq!(body, expected_response_body);
}

/// Verifies that passthrough mirroring is able to mirror multiple HTTP requests from multiple
/// connections.
///
/// In this test, [`hyper`] is used directly to send HTTP requests,
/// as it gives us more control over the underlying TCP connections.
#[rstest]
#[tokio::test]
async fn mirror_http_traffic(
    #[future]
    #[notrace]
    kube_client: KubeClient,
) {
    let kube_client = kube_client.await;
    let service = basic_service(
        &format!("e2e-{:x}-mirror-http-traffic", rand::random::<u16>(),),
        "NodePort",
        HTTP_KEEP_ALIVE_IMAGE,
        "http-echo",
        false,
        std::future::ready(kube_client.clone()),
    )
    .await;
    let portforwarder = PortForwarder::new(
        kube_client.get_client(),
        &service.pod_name,
        &service.namespace,
        80,
    )
    .await;

    let process = Application::PythonFlaskHTTP
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            None,
            None,
        )
        .await;
    process
        .wait_for_line(Duration::from_secs(120), "daemon subscribed")
        .await;

    for i in 1..=2 {
        println!("Making HTTP connection {i}");
        let mut sender = make_http_conn(&portforwarder).await;
        for j in 1..=2 {
            println!("Making HTTP request {j} in connection {i}");
            let body = format!("GET {i}:{j}");
            let expected_response_body = format!("Echo [remote]: {body}");
            let request = Request::builder()
                .method(Method::GET)
                .uri(format!("http://{}", portforwarder.address()))
                .body(body.clone())
                .unwrap();
            send_and_verify(&mut sender, request, &expected_response_body).await;
        }
    }

    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let stdout = process.get_stdout().await;
            let requests = stdout.lines().filter(|line| line.contains("GET")).count();
            match requests.cmp(&4) {
                Ordering::Equal => break,
                Ordering::Less => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Ordering::Greater => {
                    panic!("too many requests were received by the local app: {requests}")
                }
            }
        }
    })
    .await
    .expect("local mirroring app did not print expected request logs on time");
}

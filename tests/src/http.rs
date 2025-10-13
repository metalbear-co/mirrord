#![cfg(test)]

use std::{cmp::Ordering, io::Write, time::Duration};

use http_body_util::BodyExt;
use hyper::{client::conn::http1::SendRequest, Method, Request};
use hyper_util::rt::TokioIo;
use kube::Client;
use rstest::*;
use tempfile::NamedTempFile;
use tokio::net::TcpStream;

use crate::utils::{
    application::Application, kube_client, port_forwarder::PortForwarder, services::basic_service,
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
#[cfg_attr(target_os = "windows", ignore)]
#[cfg_attr(not(any(feature = "job", feature = "ephemeral")), ignore)]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(360))]
async fn mirror_http_traffic(
    #[future]
    #[notrace]
    kube_client: Client,
) {
    let mirrord_config = serde_json::json!({
        "agent": {
            "passthrough_mirroring": true,
        }
    });
    let mut mirrord_config_file = NamedTempFile::with_suffix(".json").unwrap();
    mirrord_config_file
        .write_all(mirrord_config.to_string().as_bytes())
        .unwrap();

    let kube_client = kube_client.await;
    let service = basic_service(
        &format!("e2e-{:x}-mirror-http-traffic", rand::random::<u16>(),),
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-http-keep-alive:latest",
        "http-echo",
        false,
        std::future::ready(kube_client.clone()),
    )
    .await;
    let portforwarder = PortForwarder::new(
        kube_client.clone(),
        &service.pod_name,
        &service.namespace,
        80,
    )
    .await;

    let process = Application::PythonFlaskHTTP
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(vec!["-f", mirrord_config_file.path().to_str().unwrap()]),
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

/// Verifies that passthrough mirroring can handle multiple mirroring clients and a stealing client
/// at the same time.
///
/// This test requires that the operator's agents use passthrough mirroring and connection flushing.
#[cfg_attr(not(feature = "operator"), ignore)]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
async fn concurrent_mirror_and_steal(
    #[future]
    #[notrace]
    kube_client: Client,
) {
    let kube_client = kube_client.await;
    let service = basic_service(
        &format!(
            "e2e-{:x}-concurrent-mirror-and-steal",
            rand::random::<u16>(),
        ),
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-http-keep-alive:latest",
        "http-echo",
        false,
        std::future::ready(kube_client.clone()),
    )
    .await;
    let portforwarder = PortForwarder::new(
        kube_client.clone(),
        &service.pod_name,
        &service.namespace,
        80,
    )
    .await;

    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("http://{}", portforwarder.address()))
        .header("x-filter", "yes")
        .body(String::new())
        .unwrap();

    println!("Making the first HTTP connection and sending a request to the remote service...");
    let mut sender = make_http_conn(&portforwarder).await;
    send_and_verify(&mut sender, request.clone(), "Echo [remote]: ").await;

    println!("Request to the remote service succeeded, starting the first mirroring client...");
    let mirror_client_1 = Application::PythonFlaskHTTP
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            None,
            None,
        )
        .await;
    mirror_client_1
        .wait_for_line(Duration::from_secs(120), "daemon subscribed")
        .await;
    sender
        .send_request(request.clone())
        .await
        .expect_err("existing connection should be flushed");

    println!(
        "Previous connection was flushed, starting a new connection and sending another request..."
    );
    let mut sender = make_http_conn(&portforwarder).await;
    send_and_verify(&mut sender, request.clone(), "Echo [remote]: ").await;

    println!("Request to the remote service succeded, starting the stealing client...");
    let steal_client = Application::PythonFlaskHTTP
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(vec!["--steal"]),
            Some(vec![("MIRRORD_HTTP_HEADER_FILTER", "x-filter: yes")]),
        )
        .await;
    steal_client
        .wait_for_line(Duration::from_secs(120), "daemon subscribed")
        .await;
    println!("Sending a request that should be stolen...");
    send_and_verify(&mut sender, request.clone(), "GET").await;
    sender = make_http_conn(&portforwarder).await; // stealing app might be closing the connection

    println!("Request was stolen, starting the second mirroring client...");
    let mirror_client_2 = Application::PythonFlaskHTTP
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            None,
            None,
        )
        .await;
    mirror_client_2
        .wait_for_line(Duration::from_secs(120), "daemon subscribed")
        .await;

    println!("Sending a request that should be stolen...");
    send_and_verify(&mut sender, request.clone(), "GET").await;
    sender = make_http_conn(&portforwarder).await; // stealing app might be closing the connection

    println!("Request was stolen, sending a request that should not be stolen...");
    let mut unstolen_request = request.clone();
    unstolen_request.headers_mut().remove("x-filter");
    send_and_verify(&mut sender, unstolen_request.clone(), "Echo [remote]: ").await;

    println!(
        "Request was not stolen, verifying that both mirroring clients received their requests..."
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(
            async {
                loop {
                    let stdout = mirror_client_1.get_stdout().await;
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
            },
            async {
                loop {
                    let stdout = mirror_client_2.get_stdout().await;
                    let requests = stdout.lines().filter(|line| line.contains("GET")).count();
                    match requests.cmp(&2) {
                        Ordering::Equal => break,
                        Ordering::Less => {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                        Ordering::Greater => {
                            panic!("too many requests were received by the local app: {requests}")
                        }
                    }
                }
            },
        )
    })
    .await
    .expect("one of the local mirroring apps did not print expected request logs on time");
}

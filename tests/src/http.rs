#![cfg(test)]

use std::{io::Write, time::Duration};

use http_body_util::BodyExt;
use hyper::{Method, Request};
use hyper_util::rt::{TokioExecutor, TokioIo};
use kube::Client;
use rstest::*;
use tempfile::NamedTempFile;
use tokio::net::TcpStream;

use crate::utils::{
    application::Application, kube_client, port_forwarder::PortForwarder, services::basic_service,
};

enum HttpSender {
    V1(hyper::client::conn::http1::SendRequest<String>),
    V2(hyper::client::conn::http2::SendRequest<String>),
}

impl HttpSender {
    async fn send(&mut self, request: Request<String>, expected_response_body: &str) {
        println!("Sending request");
        let response = match self {
            Self::V1(sender) => sender.send_request(request).await.unwrap(),
            Self::V2(sender) => sender.send_request(request).await.unwrap(),
        };

        let (parts, body) = response.into_parts();
        let body_result = body
            .collect()
            .await
            .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned());
        println!(
            "Received response {} with body: {:?}",
            parts.status, body_result
        );
        let body = body_result.unwrap();
        assert_eq!(body, expected_response_body);
    }
}

async fn make_http_conn(portforwarder: &PortForwarder, http2: bool) -> HttpSender {
    let tcp_conn = TcpStream::connect(portforwarder.address()).await.unwrap();
    if http2 {
        let (sender, conn) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
            .handshake::<_, String>(TokioIo::new(tcp_conn))
            .await
            .unwrap();
        tokio::spawn(conn);
        HttpSender::V2(sender)
    } else {
        let (sender, conn) = hyper::client::conn::http1::Builder::new()
            .handshake::<_, String>(TokioIo::new(tcp_conn))
            .await
            .unwrap();
        tokio::spawn(conn);
        HttpSender::V1(sender)
    }
}

#[cfg_attr(not(any(feature = "job", feature = "ephemeral")), ignore)]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
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
        &format!("E2E-mirror-http-traffic-{:x}", rand::random::<u16>()),
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
        let mut sender = make_http_conn(&portforwarder, false).await;
        for j in 1..=2 {
            println!("Making HTTP request {j} in connection {i}");
            let body = format!("GET {i}:{j}");
            let expected_response_body = format!("Echo [remote]: {body}");
            let request = Request::builder()
                .method(Method::GET)
                .uri(format!("http://{}", portforwarder.address()))
                .body(body.clone())
                .unwrap();
            sender.send(request, &expected_response_body).await;
        }
    }

    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let stdout = process.get_stdout().await;
            let requests = stdout.lines().filter(|line| line.contains("GET")).count();
            if requests == 4 {
                break;
            } else if requests > 4 {
                panic!("Too many requests received by the local application: {requests}");
            }
        }
    })
    .await
    .unwrap();
}

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
        &format!("E2E-mirror-http-traffic-{:x}", rand::random::<u16>()),
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
        .header("x-steal", "true")
        .body(String::new())
        .unwrap();

    println!("Making the first HTTP connection and sending a request to the remote service...");
    let mut sender = make_http_conn(&portforwarder, true).await;
    sender
        .send(request.clone(), &format!("Echo [remote]: "))
        .await;

    println!("Request to the remote service succeeded, starting the first mirroring client...");
    let mirror_client_1 = Application::NodeHTTP2
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
    let HttpSender::V2(sender) = &mut sender else {
        unreachable!();
    };
    sender
        .send_request(request.clone())
        .await
        .expect_err("existing connection should be flushed");

    println!(
        "Previous connection was flushed, starting a new connection and sending another request..."
    );
    let mut sender = make_http_conn(&portforwarder, true).await;
    sender.send(request.clone(), "Echo [remote]: ").await;

    println!("Request to the remote service succeded, starting the stealing client...");
    let steal_client = Application::NodeHTTP2
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
    sender
        .send(
            request.clone(),
            "<h1>Hello HTTP/2: <b>from local app</b></h1>",
        )
        .await;

    println!("Request was stolen, starting the second mirroring client...");
    let mirror_client_2 = Application::NodeHTTP2
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
    sender
        .send(
            request.clone(),
            "<h1>Hello HTTP/2: <b>from local app</b></h1>",
        )
        .await;

    println!("Request was stolen, sending a request that should not be stolen...");
    let mut unstolen_request = request.clone();
    unstolen_request.headers_mut().remove("x-filter");
    sender.send(unstolen_request, "Echo [remote]: ").await;

    println!(
        "Request was not stolen, verifying that both mirroring clients received their requests..."
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(
            async {
                loop {
                    let stdout = mirror_client_1.get_stdout().await;
                    let requests = stdout
                        .lines()
                        .filter(|line| line.starts_with("> Request "))
                        .count();
                    if requests == 4 {
                        break;
                    } else if requests > 4 {
                        panic!("The first mirroring client received too many requests: {requests}");
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            },
            async {
                loop {
                    let stdout = mirror_client_2.get_stdout().await;
                    let requests = stdout
                        .lines()
                        .filter(|line| line.starts_with("> Request "))
                        .count();
                    if requests == 2 {
                        break;
                    } else if requests > 2 {
                        panic!("The first second client received too many requests: {requests}");
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            },
        )
    })
    .await
    .unwrap();
}

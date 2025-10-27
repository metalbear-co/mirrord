#![cfg(test)]
#![cfg(feature = "job")]

use std::{cmp::Ordering, time::Duration};

use kube::Client;
use reqwest::header::HeaderMap;
use rstest::rstest;
use serde_json::json;
use tempfile::NamedTempFile;

use crate::utils::{
    application::Application, kube_client, kube_service::KubeService, send_request,
    services::basic_service,
};

/// Test mirror mode with HTTP header filter.
/// Only requests with matching headers should be mirrored.
/// All requests (matching and non-matching) should still reach the original server.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
async fn mirror_with_http_header_filter(
    #[future] basic_service: KubeService,
    #[future] kube_client: Client,
    #[values(
        Application::PythonFlaskHTTP,
        Application::PythonFastApiHTTP,
        Application::NodeHTTP
    )]
    application: Application,
) {
    let service = basic_service.await;
    let kube_client = kube_client.await;
    let portforwarder = crate::utils::port_forwarder::PortForwarder::new(
        kube_client.clone(),
        &service.pod_name,
        &service.namespace,
        80,
    )
    .await;
    let url = format!("http://{}", portforwarder.address());

    // Create mirror mode HTTP header filter config dynamically
    let config = json!({
        "feature": {
            "network": {
                "incoming": {
                    "mode": "mirror",
                    "http_filter": {
                        "header_filter": "x-filter: yes"
                    }
                }
            }
        },
        "agent": {
            "passthrough_mirroring": true,
        },
    });
    let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
    serde_json::to_writer(config_file.as_file_mut(), &config).unwrap();

    let mirror_process = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(vec!["-f", config_file.path().to_str().unwrap()]),
            None,
        )
        .await;
    
    // Wait for application-specific startup message first
    match application {
        Application::PythonFastApiHTTP => {
            mirror_process
                .wait_for_line(Duration::from_secs(120), "Application startup complete")
                .await;
        },
        Application::PythonFlaskHTTP  => {
            mirror_process
                .wait_for_line_stdout(Duration::from_secs(120), "Server listening on port 80")
                .await;
        }
        _ => {},
    };

    // Then wait for daemon subscription which happens after app startup
    mirror_process
        .wait_for_line(Duration::from_secs(120), "daemon subscribed")
        .await;

    // Send request that SHOULD be mirrored
    let client = reqwest::Client::new();
    let req_builder = client.get(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap()); // This matches our filter
    send_request(
        req_builder,
        Some("OK - GET: Request completed\n"),
        headers.clone(),
    )
    .await;

    // Send request that should NOT be mirrored
    let client = reqwest::Client::new();
    let req_builder = client.get(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "no".parse().unwrap()); // This does NOT match our filter
    send_request(
        req_builder,
        Some("OK - GET: Request completed\n"),
        headers.clone(),
    )
    .await;

    // Send request without the header
    let client = reqwest::Client::new();
    let req_builder = client.get(&url);
    let headers = HeaderMap::default(); // No x-filter header
    send_request(req_builder, Some("OK - GET: Request completed\n"), headers).await;

    // Send DELETE to close the mirror application
    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap()); // Should be mirrored and close app
    send_request(
        req_builder,
        Some("OK - DELETE: Request completed\n"),
        headers,
    )
    .await;

    // Verify that only the expected number of requests were mirrored to the local app
    // We sent 4 requests total, but only 2 should have been mirrored (those with x-filter: yes)
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let stdout = mirror_process.get_stdout().await;
            // Count only the "Request completed"
            let requests = stdout.lines().filter(|line| line.contains("Request completed")).count();
            match requests.cmp(&2) {
                Ordering::Equal => break,
                Ordering::Less => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Ordering::Greater => {
                    panic!("too many requests were received by the local app: {requests}. Expected exactly 2 (only filtered requests)")
                }
            }
        }
    })
    .await
    .expect("local mirroring app did not receive exactly the expected number of requests on time");

    // Wait for the process to exit after the DELETE request
    #[cfg(not(target_os = "windows"))]
    tokio::time::timeout(Duration::from_secs(40), mirror_process.wait())
        .await
        .unwrap();
}

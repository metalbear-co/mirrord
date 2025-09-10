#![cfg(test)]
#![cfg(feature = "job")]

use std::time::Duration;

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
#[timeout(Duration::from_secs(120))]
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
        }
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

    mirror_process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
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

    application.assert(&mirror_process).await;
}

/// Test mirror mode with HTTP path filter.
/// Only requests with matching paths should be mirrored
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
async fn mirror_with_http_path_filter(
    #[future] basic_service: KubeService,
    #[future] kube_client: Client,
    #[values(Application::NodeHTTP)] application: Application,
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

    let mirror_process = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            None,
            Some(vec![("MIRRORD_HTTP_PATH_FILTER", "/api/.*")]),
        )
        .await;

    mirror_process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    println!("Mirror process started, testing HTTP path filtering...");

    // Send request to matching path (should be mirrored)
    println!("Sending request to /api/test (should be mirrored)...");
    let client = reqwest::Client::new();
    let matching_url = format!("{}/api/test", url);
    let req_builder = client.get(&matching_url);
    send_request(
        req_builder,
        Some("OK - GET: Request completed\n"),
        HeaderMap::default(),
    )
    .await;

    // Send request to non-matching path (should NOT be mirrored)
    println!("Sending request to /other (should NOT be mirrored)...");
    let client = reqwest::Client::new();
    let non_matching_url = format!("{}/other", url);
    let req_builder = client.get(&non_matching_url);
    send_request(
        req_builder,
        Some("OK - GET: Request completed\n"),
        HeaderMap::default(),
    )
    .await;

    // Send DELETE to matching path to close the mirror application
    println!("Sending DELETE to /api/close to close mirror application...");
    let client = reqwest::Client::new();
    let close_url = format!("{}/api/close", url);
    let req_builder = client.delete(&close_url);
    send_request(
        req_builder,
        Some("OK - DELETE: Request completed\n"),
        HeaderMap::default(),
    )
    .await;

    application.assert(&mirror_process).await;
}

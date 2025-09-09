#![cfg(test)]
#![cfg(feature = "job")]

use std::time::Duration;

use kube::Client;
use reqwest::header::HeaderMap;
use rstest::rstest;
use tokio::time::timeout;

use crate::utils::{
    application::Application, config_dir, kube_client, kube_service::KubeService, send_request,
    services::basic_service,
};

/// Test mirror mode with HTTP header filter.
/// Only requests with matching headers should be mirrored.
/// All requests (matching and non-matching) should still reach the original server.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
async fn mirror_with_http_header_filter(
    config_dir: &std::path::Path,
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

    // Use mirror mode with HTTP header filter config
    let mut config_path = config_dir.to_path_buf();
    config_path.push("http_filter_header_mirror.json");

    let mirror_process = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(vec!["-f", config_path.to_str().unwrap()]),
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

/// Test that mirror mode rejects HTTP filters when not using passthrough mirroring.
/// This test verifies that sniffer mode properly rejects filtered subscriptions.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
async fn mirror_http_filter_requires_passthrough(
    config_dir: &std::path::Path,
    #[future] basic_service: KubeService,
    #[future] kube_client: Client,
    #[values(Application::PythonFlaskHTTP)] application: Application,
) {
    let service = basic_service.await;
    let _kube_client = kube_client.await;

    // Use mirror mode with HTTP filter but WITHOUT passthrough mirroring
    let mut config_path = config_dir.to_path_buf();
    config_path.push("http_filter_header_mirror.json");

    let mut mirror_process = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(vec![
                "-f",
                config_path.to_str().unwrap(),
                "--no-passthrough-mirroring", // Force sniffer mode
            ]),
            None,
        )
        .await;

    // The process should fail because HTTP filtering requires passthrough mirroring
    let exit_status = timeout(Duration::from_secs(30), mirror_process.child.wait())
        .await
        .expect("Process should exit within 30 seconds")
        .expect("Failed to get exit status");

    // Verify that the process failed (non-zero exit code)
    assert!(
        !exit_status.success(),
        "Process should fail when using HTTP filter without passthrough mirroring"
    );

    // Check that error message contains the expected text
    let stderr = mirror_process.get_stderr().await;
    assert!(
        stderr.contains("HTTP filtering in mirror mode requires passthrough mirroring"),
        "Expected error message not found in stderr: {stderr}"
    );
}

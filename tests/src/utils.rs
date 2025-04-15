#![allow(clippy::unused_io_amount)]
#![allow(clippy::indexing_slicing)]

use std::{path::PathBuf, sync::Once};

use chrono::{Timelike, Utc};
#[cfg(feature = "operator")]
use cluster_resource::operator::*;
use k8s_openapi::api::core::v1::Service;
use kube::{Client, Config};
pub use process::TestProcess;
use rand::distr::{Alphanumeric, SampleString};
use reqwest::{RequestBuilder, StatusCode};
use rstest::*;
use serde_json::{json, Value};

pub(crate) mod application;
pub(crate) mod cluster_resource;
pub(crate) mod ipv6;
pub(crate) mod kube_service;
pub(crate) mod port_forwarder;
pub mod process;
pub(crate) mod resource_guard;
pub(crate) mod run_command;
pub(crate) mod services;
pub mod sqs_resources;
pub(crate) mod watch;

const TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
pub const CONTAINER_NAME: &str = "test";

/// Name of the environment variable used to control cleanup after failed tests.
/// By default, resources from failed tests are deleted.
/// However, if this variable is set, resources will always be preserved.
pub const PRESERVE_FAILED_ENV_NAME: &str = "MIRRORD_E2E_PRESERVE_FAILED";

/// All Kubernetes resources created for testing purposes share this label.
pub const TEST_RESOURCE_LABEL: (&str, &str) = ("mirrord-e2e-test-resource", "true");

/// Creates a random string of 7 alphanumeric lowercase characters.
pub(crate) fn random_string() -> String {
    Alphanumeric
        .sample_string(&mut rand::rng(), 7)
        .to_ascii_lowercase()
}

/// Returns string with time format of hh:mm:ss
fn format_time() -> String {
    let now = Utc::now();
    format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
}

static CRYPTO_PROVIDER_INSTALLED: Once = Once::new();

#[fixture]
pub async fn kube_client() -> Client {
    CRYPTO_PROVIDER_INSTALLED.call_once(|| {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("Failed to install crypto provider");
    });

    let mut config = Config::infer().await.unwrap();
    config.accept_invalid_certs = true;
    Client::try_from(config).unwrap()
}

/// Change the `ipFamilies` and `ipFamilyPolicy` fields to make the service IPv6-only.
///
/// # Panics
///
/// Will panic if the given service does not have a spec.
fn set_ipv6_only(service: &mut Service) {
    let spec = service.spec.as_mut().unwrap();
    spec.ip_families = Some(vec!["IPv6".to_string()]);
    spec.ip_family_policy = Some("SingleStack".to_string());
}

fn default_env() -> Value {
    json!(
        [
            {
              "name": "MIRRORD_FAKE_VAR_FIRST",
              "value": "mirrord.is.running"
            },
            {
              "name": "MIRRORD_FAKE_VAR_SECOND",
              "value": "7777"
            },
            {
                "name": "MIRRORD_FAKE_VAR_THIRD",
                "value": "foo=bar"
            }
        ]
    )
}

/// Take a request builder of any method, add headers, send the request, verify success, and
/// optionally verify expected response.
pub async fn send_request(
    request_builder: RequestBuilder,
    expect_response: Option<&str>,
    headers: reqwest::header::HeaderMap,
) {
    let (client, request) = request_builder.headers(headers).build_split();
    let request = request.unwrap();
    println!(
        "Sending an HTTP request with version={:?}, method=({}), url=({}), headers=({:?})",
        request.version(),
        request.method(),
        request.url(),
        request.headers(),
    );

    let response = client.execute(request).await.unwrap();

    let status = response.status();
    let body = String::from_utf8_lossy(response.bytes().await.unwrap().as_ref()).into_owned();

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status, response body: {body}"
    );

    if let Some(expected_response) = expect_response {
        assert_eq!(body, expected_response);
    }
}

pub async fn send_requests(url: &str, expect_response: bool, headers: reqwest::header::HeaderMap) {
    // Create client for each request until we have a match between local app and remote app
    // as connection state is flaky
    println!("{url}");

    let client = reqwest::Client::new();
    let req_builder = client.get(url);
    send_request(
        req_builder,
        expect_response.then_some("GET"),
        headers.clone(),
    )
    .await;

    let client = reqwest::Client::new();
    let req_builder = client.post(url).body(TEXT);
    send_request(
        req_builder,
        expect_response.then_some("POST"),
        headers.clone(),
    )
    .await;

    let client = reqwest::Client::new();
    let req_builder = client.put(url);
    send_request(
        req_builder,
        expect_response.then_some("PUT"),
        headers.clone(),
    )
    .await;

    let client = reqwest::Client::new();
    let req_builder = client.delete(url);
    send_request(
        req_builder,
        expect_response.then_some("DELETE"),
        headers.clone(),
    )
    .await;
}

#[fixture]
#[once]
pub fn config_dir() -> PathBuf {
    let mut config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    config_path.push("configs");
    config_path
}

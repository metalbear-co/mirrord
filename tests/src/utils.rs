#![allow(clippy::unused_io_amount)]
#![allow(clippy::indexing_slicing)]

use std::{collections::BTreeMap, path::PathBuf};

use k8s_openapi::api::core::v1::Service;
use kube::{api::GroupVersionKind, discovery, Client, Resource};
use mirrord_operator::crd::MirrordOperatorCrd;
use rand::distr::{Alphanumeric, SampleString};
use reqwest::{RequestBuilder, StatusCode};
use rstest::*;
use serde_json::{json, Value};

pub mod application;
pub mod client;
pub mod cluster_resource;
pub mod images;
pub mod ipv6;
pub mod kube_service;
pub mod port_forwarder;
pub mod resource_guard;
pub mod services;

#[cfg(target_os = "windows")]
pub mod windows;

pub mod watch;

pub use client::{kube_client, KubeClient};

const TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
pub const CONTAINER_NAME: &str = "test";

/// Name of the environment variable used to control cleanup after failed tests.
/// By default, resources from failed tests are deleted.
/// However, if this variable is set, resources will always be preserved.
pub const PRESERVE_FAILED_ENV_NAME: &str = "MIRRORD_E2E_PRESERVE_FAILED";

/// All Kubernetes resources created for testing purposes share this label.
pub const TEST_RESOURCE_LABEL: (&str, &str) = ("mirrord-e2e-test-resource", "true");

/// Environment variable a CI job sets to its run id; see [`RUN_ID_LABEL`].
pub const RUN_ID_ENV: &str = "MIRRORD_E2E_RUN_ID";

/// Label carrying [`RUN_ID_ENV`], present exactly when the suite runs in CI.
pub const RUN_ID_LABEL: &str = "mirrord-e2e-run";

/// Environment variable naming who runs the suite, for a run started on purpose against a shared
/// cluster (an xtask, a developer's own invocation). A CI run needs none: [`RUN_ID_ENV`] marks
/// it as `ci`. A plain local run against a throwaway cluster sets neither and gets no labels.
pub const OWNER_ENV: &str = "MIRRORD_E2E_OWNER";

/// Label naming who created a resource: `ci` for a CI run, otherwise the [`OWNER_ENV`] value.
/// On a shared cluster it tells CI leftovers from a colleague's, and lets anyone clean up their
/// own with one selector.
pub const OWNER_LABEL: &str = "mirrord-e2e-owner";

/// The labels every resource a test creates carries: [`OWNER_LABEL`] and, in CI, [`RUN_ID_LABEL`].
/// Empty when neither [`RUN_ID_ENV`] nor [`OWNER_ENV`] is set, so a run nobody asked to be
/// identifiable leaves no trace of who ran it.
///
/// A CI job sweeps its own leftovers (tests killed on a timeout, a cancelled job) by run id,
/// including objects that live outside the test's namespace. A developer sweeps theirs by owner.
#[must_use]
pub fn origin_labels() -> BTreeMap<String, String> {
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let mut labels = BTreeMap::new();
    if let Some(run_id) = env(RUN_ID_ENV) {
        labels.insert(OWNER_LABEL.to_owned(), "ci".to_owned());
        labels.insert(RUN_ID_LABEL.to_owned(), run_id);
    } else if let Some(owner) = env(OWNER_ENV) {
        labels.insert(OWNER_LABEL.to_owned(), label_value(&owner));
    }
    labels
}

/// Shapes free text into a valid label value: lowercase alphanumerics, `-`, `_` and `.`, at
/// most 63 characters.
fn label_value(text: &str) -> String {
    let value: String = text
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' | '_' | '.' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .take(63)
        .collect();
    value
        .trim_matches(|c| c == '-' || c == '_' || c == '.')
        .to_owned()
}

pub fn get_test_resource_label_map() -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from_iter([(
        TEST_RESOURCE_LABEL.0.to_owned(),
        TEST_RESOURCE_LABEL.1.to_owned(),
    )]);
    labels.extend(origin_labels());
    labels
}

/// Creates a random string of 7 alphanumeric lowercase characters.
pub fn random_string() -> String {
    Alphanumeric
        .sample_string(&mut rand::rng(), 7)
        .to_ascii_lowercase()
}

/// Change the `ipFamilies` and `ipFamilyPolicy` fields to make the service IPv6-only.
///
/// # Panics
///
/// Will panic if the given service does not have a spec.
fn set_ipv6_only(service: &mut Service) {
    let spec = service.spec.as_mut().unwrap();
    spec.ip_families = Some(vec!["IPv6".to_owned()]);
    spec.ip_family_policy = Some("SingleStack".to_owned());
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

/// Detect if Operator is installed
#[allow(dead_code)]
pub(crate) async fn operator_installed(client: &Client) -> kube::Result<bool> {
    let gvk = GroupVersionKind {
        group: MirrordOperatorCrd::group(&()).into_owned(),
        version: MirrordOperatorCrd::version(&()).into_owned(),
        kind: MirrordOperatorCrd::kind(&()).into_owned(),
    };

    match discovery::oneshot::pinned_kind(client, &gvk).await {
        Ok(..) => Ok(true),
        Err(kube::Error::Api(response)) if response.code == 404 => Ok(false),
        Err(error) => Err(error),
    }
}

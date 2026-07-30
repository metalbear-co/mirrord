//! Tests that need a single-stack IPv6 cluster, compiled only with the `ipv6` cargo
//! feature - the regular e2e jobs never see them. The `e2e-ipv6` CI job runs them on an
//! IPv6 kind cluster; see the IPv6 section of CONTRIBUTING.md for running them locally.
#![cfg(test)]
#![cfg(feature = "ipv6")]

use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use mirrord_test_utils::run_command::run_exec_with_target;
use rstest::*;

use crate::utils::{
    application::Application,
    client::kube_client,
    ipv6::{ipv6_service, portforward_http_requests, portforward_http_requests_with_body},
    kube_service::KubeService,
    KubeClient,
};

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(240))]
async fn steal_http_ipv6_traffic(
    #[future] ipv6_service: KubeService,
    #[future] kube_client: KubeClient,
) {
    let application = Application::PythonFastApiHTTPIPv6;
    let service = ipv6_service.await;
    let kube_client = kube_client.await;

    let mut flags = vec!["--steal"];

    if cfg!(feature = "ephemeral") {
        flags.extend(["-e"].into_iter());
    }

    let mut process = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(flags),
            None,
        )
        .await;

    #[cfg(target_os = "windows")]
    application.wait_until_listening(&process).await;

    #[cfg(not(target_os = "windows"))]
    process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    let api = Api::<Pod>::namespaced(kube_client.get_client(), &service.namespace);
    portforward_http_requests(&api, service).await;

    tokio::time::timeout(Duration::from_secs(40), process.wait())
        .await
        .unwrap();

    application.assert(&process).await;
}

/// Mirror counterpart of `steal_http_ipv6_traffic` - the default incoming mode with no
/// `ipv6` key in any config. The local app exits only after receiving the mirrored
/// DELETE request, so a successful wait proves traffic was mirrored over IPv6.
#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(240))]
async fn mirror_http_ipv6_traffic(
    #[future] ipv6_service: KubeService,
    #[future] kube_client: KubeClient,
) {
    let application = Application::PythonFastApiHTTPIPv6;
    let service = ipv6_service.await;
    let kube_client = kube_client.await;

    let mut process = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            None,
            None,
        )
        .await;

    #[cfg(target_os = "windows")]
    application.wait_until_listening(&process).await;

    #[cfg(not(target_os = "windows"))]
    process
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    let api = Api::<Pod>::namespaced(kube_client.get_client(), &service.namespace);
    portforward_http_requests_with_body(&api, service, |method| {
        format!("OK - {method}: Request completed\n")
    })
    .await;

    tokio::time::timeout(Duration::from_secs(40), process.wait())
        .await
        .unwrap();

    application.assert(&process).await;
}

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(30))]
async fn connect_to_kubernetes_api_service_over_ipv6() {
    let app = Application::CurlToKubeApi;
    let mut process = app.run_targetless(None, None, None).await;
    let res = process.wait().await;
    assert!(res.success());
    let stdout = process.get_stdout().await;
    assert!(stdout.contains(r#""apiVersion": "v1""#))
}

/// IPv6 is enabled by default, so this runs with no extra config.
/// Always ignored: on top of an IPv6 cluster it needs IPv6 internet egress, which local
/// kind clusters and CI runners don't have - use e.g. the EKS IPv6 blueprint and run it
/// with `--run-ignored all`.
#[rstest]
#[tokio::test]
#[ignore]
async fn outgoing_traffic_single_request_ipv6_enabled(#[future] ipv6_service: KubeService) {
    let service = ipv6_service.await;
    let node_command = [
        "node",
        "node-e2e/outgoing/test_outgoing_traffic_single_request_ipv6.mjs",
    ]
    .map(String::from)
    .to_vec();
    let mut process = run_exec_with_target(
        node_command,
        &service.pod_container_target(),
        None,
        None,
        None,
    )
    .await;

    let res = process.wait().await;
    assert!(res.success());
}

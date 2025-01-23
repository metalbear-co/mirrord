#![cfg(test)]
#![cfg(feature = "operator")]
//! Test concurrent stealing features with an operator.

use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use kube::Client;
use reqwest::header::HeaderMap;
use rstest::*;

use crate::utils::{
    config_dir, get_instance_name, get_service_url, kube_client, send_request, service,
    Application, KubeService,
};

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn two_clients_steal_same_target(
    #[future]
    #[notrace]
    service: KubeService,
    #[future]
    #[notrace]
    kube_client: Client,
    #[values(Application::PythonFlaskHTTP)] application: Application,
) {
    let (service, client) = tokio::join!(service, kube_client);

    let flags = vec!["--steal", "--fs-mode=local"];

    let mut client_a = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(flags.clone()),
            None,
        )
        .await;

    client_a
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    let mut client_b = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(flags),
            None,
        )
        .await;

    let res = client_b.child.wait().await.unwrap();
    assert!(!res.success());
    assert!(!client_b.get_stderr().await.contains("daemon subscribed"));

    // check if client_a is stealing
    let url = get_service_url(client, &service).await;
    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap());
    send_request(req_builder, Some("DELETE"), headers.clone()).await;

    tokio::time::timeout(Duration::from_secs(15), client_a.wait())
        .await
        .unwrap();

    client_a
        .assert_stdout_contains("DELETE: Request completed")
        .await;
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn two_clients_steal_same_target_pod_deployment(
    #[future]
    #[notrace]
    service: KubeService,
    #[future]
    #[notrace]
    kube_client: Client,
    #[values(Application::PythonFlaskHTTP)] application: Application,
) {
    let (service, client) = tokio::join!(service, kube_client);

    let flags = vec!["--steal", "--fs-mode=local"];

    let mut client_a = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(flags.clone()),
            None,
        )
        .await;

    client_a
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    let target = get_instance_name::<Deployment>(client.clone(), &service.name, &service.namespace)
        .await
        .unwrap();

    let mut client_b = application
        .run(
            &format!("deployment/{target}"),
            Some(&service.namespace),
            Some(flags.clone()),
            None,
        )
        .await;

    let res = client_b.child.wait().await.unwrap();
    assert!(!res.success());
    assert!(!client_b.get_stderr().await.contains("daemon subscribed"));

    // check if client_a is stealing
    let url = get_service_url(client, &service).await;
    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap());
    send_request(req_builder, Some("DELETE"), headers.clone()).await;

    tokio::time::timeout(Duration::from_secs(15), client_a.wait())
        .await
        .unwrap();

    client_a
        .assert_stdout_contains("DELETE: Request completed")
        .await;
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn two_clients_steal_with_http_filter(
    config_dir: &std::path::Path,
    #[future] service: KubeService,
    #[future] kube_client: Client,
    #[values(Application::NodeHTTP)] application: Application,
) {
    let service = service.await;
    let kube_client = kube_client.await;
    let url = get_service_url(kube_client.clone(), &service).await;

    let mut config_path = config_dir.to_path_buf();
    config_path.push("http_filter_header.json");

    let flags = vec!["--steal", "--fs-mode=local"];

    let mut client_a = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(flags.clone()),
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    client_a
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    let mut config_path = config_dir.to_path_buf();
    config_path.push("http_filter_header_no.json");

    let mut client_b = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(flags),
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    client_b
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap());

    send_request(req_builder, Some("DELETE"), headers.clone()).await;

    tokio::time::timeout(Duration::from_secs(10), client_a.wait())
        .await
        .unwrap();

    client_a
        .assert_stdout_contains("DELETE: Request completed")
        .await;

    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "no".parse().unwrap());

    send_request(req_builder, Some("DELETE"), headers.clone()).await;

    tokio::time::timeout(Duration::from_secs(10), client_b.wait())
        .await
        .unwrap();

    client_b
        .assert_stdout_contains("DELETE: Request completed")
        .await;
}

#![cfg(test)]
#![cfg(feature = "operator")]
//! Test concurrent stealing features with an operator.

use std::time::Duration;

use kube::Client;
use reqwest::header::HeaderMap;
use rstest::*;

use crate::utils::{
    application::Application, config_dir, kube_client, kube_service::KubeService,
    port_forwarder::PortForwarder, send_request, services::basic_service,
};

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn two_clients_steal_same_target(
    #[future]
    #[notrace]
    basic_service: KubeService,
    #[future]
    #[notrace]
    kube_client: Client,
    #[values(crate::utils::application::Application::PythonFlaskHTTP)] application: Application,
) {
    let (service, client) = tokio::join!(basic_service, kube_client);

    let flags = vec!["--steal", "--fs-mode=local"];

    let client_a = application
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
    println!("Client A subscribed the port");

    let mut client_b = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(flags),
            None,
        )
        .await;

    println!("Waiting for client B to crash");
    let res = client_b.child.wait().await.unwrap();
    assert!(!res.success());
    assert!(!client_b.get_stderr().await.contains("daemon subscribed"));

    // check if client_a is stealing
    let portforwarder =
        PortForwarder::new(client.clone(), &service.pod_name, &service.namespace, 80).await;
    let url = format!("http://{}", portforwarder.address());
    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap());
    send_request(req_builder, Some("DELETE"), headers.clone()).await;
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn two_clients_steal_same_target_pod_deployment(
    #[future]
    #[notrace]
    basic_service: KubeService,
    #[future]
    #[notrace]
    kube_client: Client,
    #[values(Application::PythonFlaskHTTP)] application: Application,
) {
    let (service, client) = tokio::join!(basic_service, kube_client);

    let flags = vec!["--steal", "--fs-mode=local"];

    let client_a = application
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
    println!("Client A subscribed the port");

    let deployment_name = service.deployment.metadata.name.as_deref().unwrap();

    let mut client_b = application
        .run(
            &format!("deployment/{deployment_name}"),
            Some(&service.namespace),
            Some(flags.clone()),
            None,
        )
        .await;

    println!("Waiting for client B to crash");
    let res = client_b.child.wait().await.unwrap();
    assert!(!res.success());
    assert!(!client_b.get_stderr().await.contains("daemon subscribed"));

    // check if client_a is stealing
    let portforwarder =
        PortForwarder::new(client.clone(), &service.pod_name, &service.namespace, 80).await;
    let url = format!("http://{}", portforwarder.address());
    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap());
    send_request(req_builder, Some("DELETE"), headers.clone()).await;
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn two_clients_steal_with_http_filter(
    config_dir: &std::path::Path,
    #[future] basic_service: KubeService,
    #[future] kube_client: Client,
    #[values(Application::NodeHTTP)] application: Application,
) {
    let service = basic_service.await;
    let kube_client = kube_client.await;
    let portforwarder = PortForwarder::new(
        kube_client.clone(),
        &service.pod_name,
        &service.namespace,
        80,
    )
    .await;
    let url = format!("http://{}", portforwarder.address());

    let mut config_path = config_dir.to_path_buf();
    config_path.push("http_filter_header.json");

    let flags = vec!["--steal", "--fs-mode=local"];

    let client_a = application
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
    println!("Client A subscribed the port");

    let mut config_path = config_dir.to_path_buf();
    config_path.push("http_filter_header_no.json");

    let client_b = application
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
    println!("Client B subscribed the port");

    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "yes".parse().unwrap());
    send_request(req_builder, Some("DELETE"), headers.clone()).await;

    let client = reqwest::Client::new();
    let req_builder = client.delete(&url);
    let mut headers = HeaderMap::default();
    headers.insert("x-filter", "no".parse().unwrap());
    send_request(req_builder, Some("DELETE"), headers.clone()).await;
}

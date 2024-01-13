#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

use std::{collections::BTreeMap, time::Duration};

use aws_sdk_sqs::types::{
    QueueAttributeName,
    QueueAttributeName::{FifoQueue, MessageRetentionPeriod},
};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{PodSpec, PodTemplateSpec},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client};
use mirrord_operator::crd::QueueConsumer::Deployment;
use reqwest::header::HeaderMap;
use rstest::*;
use tokio_tungstenite::tungstenite::client;
use mirrord_operator::crd::MirrordPolicy;

use crate::utils::{config_dir, kube_client, microservice_from_deployment, send_request, service, Application, KubeService, TEST_RESOURCE_LABEL, ResourceGuard};

const SQS_CONFIG_MAP_NAME: &str = "mirrord-e2e-test-sqs-splitting";

fn test_resource_label() -> (String, String) {
    (
        TEST_RESOURCE_LABEL.0.to_string(),
        TEST_RESOURCE_LABEL.1.to_string(),
    )
}

fn metadata_with_app_label(name: String) -> ObjectMeta {
    ObjectMeta {
        labels: Some(BTreeMap::from([
            test_resource_label(),
            ("app".to_string(), name.clone()),
        ])),
        ..Default::default()
    }
}

fn metadata_with_name(name: String) -> ObjectMeta {
    ObjectMeta {
        labels: Some(BTreeMap::from([
            test_resource_label(),
            ("app".to_string(), name.clone()),
        ])),
        name: Some(name.clone()),
        ..Default::default()
    }
}

async fn deploy_test_microservice(
    kube_client: Client,
    name: String,
    queue_name: String,
    queue_url: String,
) -> KubeService {
    let dep = Deployment {
        metadata: metadata_with_name(name.clone()),
        spec: Some(DeploymentSpec {
            replicas: Some(2),
            selector: Default::default(),
            template: PodTemplateSpec {
                metadata: Some(metadata_with_app_label(name.clone())),
                spec: Some(PodSpec {
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        status: None,
    };
    microservice_from_deployment(kube_client, dep, None, None).await
}

#[fixture]
fn sqs_queue_name() -> String {
    format!(
        "MirrordE2ESplitterTests-{}.fifo",
        crate::utils::random_string()
    )
}

/// Create a new SQS fifo queue with a randomized name, return queue name and url.
#[fixture]
async fn sqs_queue(sqs_queue_name: String) -> (String, String) {
    let shared_config = aws_config::load_from_env().await;
    let client = aws_sdk_sqs::Client::new(&shared_config);
    let queue = client
        .create_queue()
        .queue_name(sqs_queue_name.clone())
        .attributes(MessageRetentionPeriod, "3600") // delete messages after an hour.
        .attributes(FifoQueue, "true") // Fifo for predictable test scenarios
        .send()
        .await
        .unwrap();
    (sqs_queue_name, queue.queue_url.unwrap())
}

pub async fn create_config_map(kube_client: Client, namespace: &str, sqs_queue_name: String) -> ResourceGuard {
    let config_map_api: Api<ConfigMap> = Api::namespaced(kube_client.clone(), namespace);
    let config_map = ConfigMap {
        binary_data: None,
        data: None,
        immutable: None,
        metadata: Default::default(),
    };
    ResourceGuard::create(
        config_map_api,
        SQS_CONFIG_MAP_NAME.to_string(),
        &config_map,
        true,
    )
        .await
        .expect("Could not create policy in E2E test."),
}

#[fixture]
pub async fn sqs_consumer_service(#[future] kube_client: Client) -> KubeService {
    // By using a different namespace for each test run, we're able to use a constant ConfigMap
    // name, and don't have to pass that information to the test service.
    let namespace = format!("e2e-tests-sqs-splitting-{}", crate::utils::random_string());
    let service = service(
        &namespace,
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-node-udp-logger:latest", // TODO
        "udp-logger",
        true,
        kube_client,
    )
    .await;
}

/// Define a queue splitter for a deployment. Start two services that both consume from an SQS
/// queue, send some messages to the queue, verify that each of the applications running with
/// mirrord get exactly the messages they are supposed to, and that the deployed application gets
/// the rest.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn two_users_one_queue(
    config_dir: &std::path::PathBuf,
    #[future] kube_client: Client,
    #[future] sqs_consumer_service: KubeService,
    sqs_queue: (String, String),
) {
    let application = Application::Go21HTTP;
    let kube_client = kube_client.await;
    let (sqs_queue_name, sqs_queue_url) = sqs_queue;

    let deployed = sqs_consumer_service.await;

    let _config_map_guard = create_config_map(kube_client.clone(), &deployed.namespace, sqs_queue_name);

    let mut config_path = config_dir.clone();
    config_path.push("sqs_queue_splitting.toml");

    let mut client_a = application
        .run(
            &deployed.target, // TODO: target the deployment maybe?
            Some(&deployed.namespace),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    client_a
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    let mut config_path = config_dir.clone();
    config_path.push("http_filter_header_no.json");

    let mut client_b = application
        .run(
            &service.target,
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

#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

use std::time::Duration;

use aws_sdk_sqs::types::QueueAttributeName::{FifoQueue, MessageRetentionPeriod};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client};
use reqwest::header::HeaderMap;
use rstest::*;

use crate::utils::{
    config_dir, kube_client, send_request, service, Application, KubeService, ResourceGuard,
    TEST_RESOURCE_LABEL,
};

const SQS_CONFIG_MAP_NAME: &str = "mirrord-e2e-test-sqs-splitting";

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

pub async fn create_config_map(
    kube_client: Client,
    namespace: &str,
    sqs_queue_name: String,
) -> ResourceGuard {
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
    .expect("Could not create policy in E2E test.")
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
    #[future] sqs_queue: (String, String),
) {
    let application = Application::Go21HTTP;
    let kube_client = kube_client.await;
    let (sqs_queue_name, sqs_queue_url) = sqs_queue.await;

    let deployed = sqs_consumer_service.await;

    // Create the config map that the remote deployment uses as the source of the queue's name.
    let _config_map_guard =
        create_config_map(kube_client.clone(), &deployed.namespace, sqs_queue_name);

    let mut config_path = config_dir.clone();
    config_path.push("sqs_queue_splitting_a.json");

    let mut client_a = application
        .run(
            &deployed.target, // TODO: target the deployment maybe?
            Some(&deployed.namespace),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    let mut config_path = config_dir.clone();
    config_path.push("sqs_queue_splitting_b.json");

    let mut client_b = application
        .run(
            &deployed.target, // TODO: target the deployment maybe?
            Some(&deployed.namespace),
            None,
            Some(vec![("MIRRORD_CONFIG_FILE", config_path.to_str().unwrap())]),
        )
        .await;

    // TODO: send messages to the original queue.

    // TODO: wait here for both clients to exit and assert exit status 0 in each of them.
    //   the test application consumes messages and verifies exact expected messages.

    // TODO: read output queue and verify that exactly the expected messages were
    //   consumed forwarded.
}

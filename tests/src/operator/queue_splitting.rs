#![cfg(test)]
#![cfg(feature = "operator")]
//! Test queue splitting features with an operator.

use std::collections::{BTreeMap, HashMap};

use aws_sdk_sqs::types::QueueAttributeName::{FifoQueue, MessageRetentionPeriod};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client};
use mirrord_operator::crd::{
    MirrordQueueSplitter, MirrordQueueSplitterSpec, QueueConsumer, QueueNameSource, SplitQueue,
};
use rstest::*;
use tokio::join;

use crate::utils::{
    config_dir, kube_client, service, service_with_env, Application, KubeService, ResourceGuard,
};

const SQS_CONFIG_MAP_NAME: &str = "mirrord-e2e-test-sqs-splitting";
const SQS_CONFIG_MAP_KEY_NAME: &str = "queue_name";
const QUEUE_SPLITTER_RESOURCE_NAME: &str = "mirrord-e2e-test-queue-splitter";

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
        data: Some(BTreeMap::from([(
            SQS_CONFIG_MAP_KEY_NAME.to_string(),
            sqs_queue_name,
        )])),
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
    .expect("Could not create config map in E2E test.")
}

/// Create the `MirrordQueueSplitter` K8s resource and a resource guard to delete it when done.
pub async fn create_splitter_resource(
    kube_client: Client,
    namespace: &str,
    deployment_name: &str,
) -> ResourceGuard {
    let qs_api: Api<MirrordQueueSplitter> = Api::namespaced(kube_client.clone(), namespace);
    let queue_splitter = MirrordQueueSplitter::new(
        QUEUE_SPLITTER_RESOURCE_NAME,
        MirrordQueueSplitterSpec {
            queues: HashMap::from([(
                "e2e-test-queue".to_string(),
                SplitQueue::Sqs(QueueNameSource::ConfigMap {
                    name: SQS_CONFIG_MAP_NAME.to_string(),
                    queue_name_key: SQS_CONFIG_MAP_KEY_NAME.to_string(),
                    sub_key: None,
                }),
            )]),
            consumer: QueueConsumer::Deployment(deployment_name.to_string()),
        },
    );
    ResourceGuard::create(
        qs_api,
        QUEUE_SPLITTER_RESOURCE_NAME.to_string(),
        &queue_splitter,
        true,
    )
    .await
    .expect("Could not create queue splitter in E2E test.")
}

#[fixture]
pub async fn sqs_consumer_service(#[future] kube_client: Client) -> KubeService {
    // By using a different namespace for each test run, we're able to use a constant ConfigMap
    // name, and don't have to pass that information to the test service.
    let namespace = format!("e2e-tests-sqs-splitting-{}", crate::utils::random_string());
    service_with_env(
        &namespace,
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-node-udp-logger:latest", // TODO
        "queue-forwarder",
        false,
        kube_client,
        serde_json::json!([
            // TODO
        ]),
    )
    .await
}

/// Send 6 messages to the original queue, such that 2 will reach each mirrord run and 2 the
/// deployed app.
async fn write_sqs_messages(queue_url: &str) {
    // TODO
}

/// This test creates a new sqs_queue with a random name and credentials from env.
///
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
    let application = Application::Go21HTTP; // TODO
    let kube_client = kube_client.await;
    let (sqs_queue_name, sqs_queue_url) = sqs_queue.await;

    let deployed = sqs_consumer_service.await;

    // Create the config map that the remote deployment uses as the source of the queue's name.
    let _config_map_guard =
        create_config_map(kube_client.clone(), &deployed.namespace, sqs_queue_name);

    // Create the `MirrordQueueSplitter` to be used in the client configurations.
    let _splitter_resource_guard =
        create_splitter_resource(kube_client.clone(), &deployed.namespace, &deployed.name).await;

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

    write_sqs_messages(&sqs_queue_url).await;

    // The test application consumes messages and verifies exact expected messages.
    join!(
        client_a.wait_assert_success(),
        client_b.wait_assert_success()
    );

    // TODO: read output queue and verify that exactly the expected messages were
    //   consumed and forwarded.
}

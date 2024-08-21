#![cfg(test)]
#![cfg(feature = "operator")]

use std::collections::BTreeMap;

use aws_sdk_sqs::types::QueueAttributeName::{FifoQueue, MessageRetentionPeriod};
use kube::{api::PostParams, Api, Client};
use mirrord_operator::crd::{
    MirrordWorkloadQueueRegistry, MirrordWorkloadQueueRegistrySpec, QueueConsumer,
    QueueConsumerType, QueueNameSource, SplitQueue, SqsQueueDetails,
};
use rstest::fixture;

use crate::utils::{kube_client, service_with_env, KubeService};

/// Name of the environment variable that holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue to read from.
const QUEUE_NAME_ENV_VAR2: &str = "SQS_TEST_Q_NAME2";

const QUEUE_SPLITTER_RESOURCE_NAME: &str = "mirrord-e2e-test-queue-splitter";

pub struct QueueInfo {
    name: String,
    url: String,
}

/// K8s resources will be deleted when this is dropped.
pub struct SqsTestResources {
    k8s_service: KubeService,
    queue1: QueueInfo,
    queue2: QueueInfo,
}

impl SqsTestResources {
    pub fn namespace(&self) -> &str {
        self.k8s_service.namespace.as_str()
    }

    pub fn queue1(&self) -> &QueueInfo {
        &self.queue1
    }

    pub fn queue2(&self) -> &QueueInfo {
        &self.queue2
    }

    pub fn deployment_target(&self) -> String {
        self.k8s_service.deployment_target()
    }
}

/// Create a new SQS fifo queue with a randomized name, return queue name and url.
async fn sqs_queue(fifo: bool) -> QueueInfo {
    let name = format!(
        "MirrordE2ESplitterTests-{}{}",
        crate::utils::random_string(),
        if fifo { ".fifo" } else { "" }
    );
    println!("Creating SQS queue: {name}");
    let shared_config = aws_config::load_from_env().await;
    let client = aws_sdk_sqs::Client::new(&shared_config);
    let mut builder = client
        .create_queue()
        .queue_name(name.clone())
        .attributes(MessageRetentionPeriod, "3600"); // delete messages after an hour.

    // Cannot set FifoQueue to false: https://github.com/aws/aws-cdk/issues/8550
    if fifo {
        builder = builder.attributes(FifoQueue, "true")
    }
    let queue = builder.send().await.unwrap();
    QueueInfo {
        name,
        url: queue.queue_url.unwrap(),
    }
}

/// Create the `MirrordWorkloadQueueRegistry` K8s resource and a resource guard to delete it when
/// done.
pub async fn create_queue_registry_resource(
    kube_client: &Client,
    namespace: &str,
    deployment_name: &str,
) {
    let qr_api: Api<MirrordWorkloadQueueRegistry> = Api::namespaced(kube_client.clone(), namespace);
    let queue_registry = MirrordWorkloadQueueRegistry::new(
        QUEUE_SPLITTER_RESOURCE_NAME,
        MirrordWorkloadQueueRegistrySpec {
            queues: BTreeMap::from([
                (
                    "e2e-test-queue1".to_string(),
                    SplitQueue::Sqs(SqsQueueDetails {
                        name_source: QueueNameSource::EnvVar(QUEUE_NAME_ENV_VAR1.to_string()),
                        tags: None,
                    }),
                ),
                (
                    "e2e-test-queue2".to_string(),
                    SplitQueue::Sqs(SqsQueueDetails {
                        name_source: QueueNameSource::EnvVar(QUEUE_NAME_ENV_VAR2.to_string()),
                        tags: None,
                    }),
                ),
            ]),
            consumer: QueueConsumer {
                name: deployment_name.to_string(),
                container: None,
                workload_type: QueueConsumerType::Deployment,
            },
        },
    );
    qr_api
        .create(&PostParams::default(), &queue_registry)
        .await
        .expect("Could not create queue splitter in E2E test.");
}

async fn sqs_consumer_service(
    kube_client: &Client,
    queue1: &QueueInfo,
    queue2: &QueueInfo,
) -> KubeService {
    let namespace = format!("e2e-tests-sqs-splitting-{}", crate::utils::random_string());
    service_with_env(
        &namespace,
        "ClusterIP",
        "docker.io/t4lz/sqs-printer:8.14", // TODO
        "queue-forwarder",
        false,
        kube_client.clone(),
        serde_json::json!([
            {
              "name": QUEUE_NAME_ENV_VAR1,
              "value": queue1.name.as_str()
            },
            {
              "name": QUEUE_NAME_ENV_VAR2,
              "value": queue2.name.as_str()
            },
            // TODO: Don't set if there is no localstack service.
            // TODO: dynamically determine localstack url?
            {
              "name": "AWS_ENDPOINT_URL",
              "value": "http://localstack:31566"
            }
        ]),
    )
    .await
}

#[fixture]
pub async fn sqs_test_resources(#[future] kube_client: Client) -> SqsTestResources {
    let kube_client = kube_client.await;
    let queue1 = sqs_queue(false).await;
    let queue2 = sqs_queue(true).await;
    let k8s_service = sqs_consumer_service(&kube_client, &queue1, &queue2).await;
    SqsTestResources {
        k8s_service,
        queue1,
        queue2,
    }
}

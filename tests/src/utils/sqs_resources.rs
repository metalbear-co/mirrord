#![cfg(test)]
#![cfg(feature = "operator")]

use std::collections::{BTreeMap, HashMap};

use aws_sdk_sqs::types::{
    MessageAttributeValue,
    QueueAttributeName::{FifoQueue, MessageRetentionPeriod},
};
use futures_util::FutureExt;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{EnvVar, Service},
};
use kube::{
    api::{Patch, PatchParams, PostParams},
    Api, Client, Resource,
};
use mirrord_operator::{
    crd::{
        MirrordWorkloadQueueRegistry, MirrordWorkloadQueueRegistrySpec, QueueConsumer,
        QueueConsumerType, QueueNameSource, SplitQueue, SqsQueueDetails,
    },
    setup::OPERATOR_NAME,
};
use rstest::fixture;

use crate::utils::{
    get_pod_or_node_host, kube_client, service_with_env, KubeService, ResourceGuard,
};

/// Name of the environment variable that holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue to read from.
const QUEUE_NAME_ENV_VAR2: &str = "SQS_TEST_Q_NAME2";

const QUEUE_SPLITTER_RESOURCE_NAME: &str = "mirrord-e2e-test-queue-splitter";

const LOCALSTACK_PATCH_LABEL_NAME: &str = "sqs-e2e-tests-localstack-patch";

const LOCALSTACK_ENDPOINT_URL: &str = "http://localstack.default.svc.cluster.local:4566";

pub struct QueueInfo {
    pub name: String,
    pub url: String,
}

/// K8s resources will be deleted when this is dropped.
pub struct SqsTestResources {
    k8s_service: KubeService,
    queue1: QueueInfo,
    queue2: QueueInfo,
    sqs_client: aws_sdk_sqs::Client,
    _guards: Vec<ResourceGuard>,
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

    pub fn sqs_client(&self) -> &aws_sdk_sqs::Client {
        &self.sqs_client
    }
}

async fn get_sqs_client(localstack_url: Option<String>) -> aws_sdk_sqs::Client {
    let mut config = aws_config::from_env();
    if let Some(endpoint_url) = localstack_url {
        println!("Using endpoint URL {endpoint_url}");
        config = config
            .endpoint_url(endpoint_url)
            .region(aws_types::region::Region::new("us-east-1"));
    }
    let config = config.load().await;
    aws_sdk_sqs::Client::new(&config)
}

/// Create a new SQS fifo queue with a randomized name, return queue name and url.
/// Also create another SQS queue that has a name that is "Echo" + the name of the first queue.
/// Create resource guards for both queues that delete the queue on drop, push into guards vec.
async fn random_name_sqs_queue_with_echo_queue(
    fifo: bool,
    client: &aws_sdk_sqs::Client,
    guards: &mut Vec<ResourceGuard>,
) -> QueueInfo {
    let q_name = format!(
        "MirrordE2ESplitterTests-{}{}",
        crate::utils::random_string(),
        if fifo { ".fifo" } else { "" }
    );

    let echo_q_name = format!("Echo{q_name}");
    // Create queue and resource guard, don't return queue details of echo queue. The deployed
    // application derives its name from the other queue.
    sqs_queue(fifo, client, guards, echo_q_name).await;

    sqs_queue(fifo, client, guards, q_name).await
}

/// Create a new SQS fifo queue, return queue name and url.
/// Create resource guard that deletes the queue on drop, push into guards vec.
async fn sqs_queue(
    fifo: bool,
    client: &aws_sdk_sqs::Client,
    guards: &mut Vec<ResourceGuard>,
    name: String,
) -> QueueInfo {
    println!("Creating SQS queue: {name}");
    let mut builder = client
        .create_queue()
        .queue_name(&name)
        .attributes(MessageRetentionPeriod, "3600"); // delete messages after an hour.

    // Cannot set FifoQueue to false: https://github.com/aws/aws-cdk/issues/8550
    if fifo {
        builder = builder.attributes(FifoQueue, "true")
    }

    let queue_url = builder.send().await.unwrap().queue_url.unwrap();
    let url = queue_url.clone();
    let queue_name = name.clone();
    let client = client.clone();

    let deleter = Some(
        async move {
            println!("deleting SQS queue {queue_name}");
            if let Err(err) = client.delete_queue().queue_url(queue_url).send().await {
                eprintln!("Could not delete SQS queue {queue_name}: {err:?}");
            }
        }
        .boxed(),
    );
    guards.push(ResourceGuard {
        delete_on_fail: true,
        deleter,
    });

    QueueInfo { name, url }
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

/// Create a microservice for the sqs test application.
/// That service will be configured to use the "localstack" service in the "default" namespace for
/// all AWS stuff.
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
            {
              "name": "AWS_ENDPOINT_URL",
              "value": LOCALSTACK_ENDPOINT_URL
            },
            {
              "name": "AWS_REGION",
              "value": "us-east-1"
            },
            // All these auth variables must be set to something, but it doesn't matter to what.
            {
              "name": "AWS_SECRET_ACCESS_KEY",
              "value": "whatever"
            },
            {
              "name": "AWS_ACCESS_KEY_ID",
              "value": "pineapple"
            },
            {
              "name": "AWS_SECRET_KEY",
              "value": "boats"
            },
        ]),
    )
    .await
}

const AWS_ENV: [(&str, &str); 5] = [
    ("AWS_ENDPOINT_URL", LOCALSTACK_ENDPOINT_URL),
    ("AWS_REGION", "us-east-1"),
    ("AWS_SECRET_ACCESS_KEY", "whatever"),
    ("AWS_SECRET_ACCESS_KEY_ID", "pineapple"),
    ("AWS_SECRET_KEY", "boats"),
];

/// Take env from deployment, return container index and env
fn take_env(deployment: Deployment) -> (usize, Vec<EnvVar>) {
    let (index, container) = deployment
        .spec
        .unwrap()
        .template
        .spec
        .unwrap()
        .containers
        .into_iter()
        .enumerate()
        .find(|(_, container)| container.name == OPERATOR_NAME)
        .expect("could not find right operator container");
    (index, container.env.unwrap_or_default())
}

/// Patch the deployed operator to use localstack for all AWS API calls.
/// Return a ResourceGuard that will restore the original env when dropped.
async fn patch_operator_for_localstack(client: &Client, guards: &mut Vec<ResourceGuard>) {
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), "mirrord");

    let operator_deploy = deploy_api
        .get(OPERATOR_NAME)
        .await
        .expect(r#"Did not find the operator's deployment in the "mirrord" namespace."#);

    if let Some(label_map) = operator_deploy.meta().labels.as_ref() {
        if label_map.contains_key(LOCALSTACK_PATCH_LABEL_NAME) {
            println!("operator already patched for localstack, not patching.");
            return;
        }
    };

    let (container_index, mut env) = take_env(operator_deploy);

    let mut aws_env = HashMap::from(AWS_ENV);
    let original_env = env.clone();
    for env_var in env.iter_mut() {
        if let Some(aws_value) = aws_env.remove(env_var.name.as_str()) {
            env_var.value = Some(aws_value.to_string())
        }
    }
    for (name, value) in aws_env {
        env.push(EnvVar {
            name: name.to_string(),
            value: Some(value.to_string()),
            value_from: None,
        })
    }

    let container_index = container_index.to_string();
    let path = jsonptr::Pointer::new(&[
        "spec",
        "template",
        "spec",
        "containers",
        container_index.as_str(),
        "env",
    ]);
    let env_path = path.clone();
    let value = serde_json::to_value(env).unwrap();

    let label_path = jsonptr::Pointer::new(["metadata", "labels", LOCALSTACK_PATCH_LABEL_NAME]);

    let _operator_deploy = deploy_api
        .patch(
            OPERATOR_NAME,
            &PatchParams::default(),
            &Patch::<Deployment>::Json(json_patch::Patch(vec![
                json_patch::PatchOperation::Replace(json_patch::ReplaceOperation { path, value }),
                json_patch::PatchOperation::Add(json_patch::AddOperation {
                    path: label_path.clone(),
                    value: serde_json::to_value("true").unwrap(),
                }),
            ])),
        )
        .await
        .unwrap();

    let container_name_path = jsonptr::Pointer::new(&[
        "spec",
        "template",
        "spec",
        "containers",
        container_index.as_str(),
        "name",
    ]);

    let restore_env = async move {
        println!("Restoring AWS env vars from before SQS tests");
        if let Err(err) = deploy_api
            .patch(
                OPERATOR_NAME,
                &PatchParams::default(),
                &Patch::<Deployment>::Json(json_patch::Patch(vec![
                    json_patch::PatchOperation::Test(json_patch::TestOperation {
                        path: container_name_path,
                        value: serde_json::to_value(OPERATOR_NAME).unwrap(),
                    }),
                    json_patch::PatchOperation::Replace(json_patch::ReplaceOperation {
                        path: env_path,
                        value: serde_json::to_value(&original_env).unwrap(),
                    }),
                    json_patch::PatchOperation::Remove(json_patch::RemoveOperation {
                        path: label_path,
                    }),
                ])),
            )
            .await
        {
            println!("Failed to patch operator deployment to restore AWS env: {err:?}");
        }
    };

    guards.push(ResourceGuard {
        delete_on_fail: true,
        deleter: Some(restore_env.boxed()),
    });
}

async fn localstack_endpoint_external_url(kube_client: &Client) -> String {
    let localstack_host = get_pod_or_node_host(kube_client.clone(), "localstack", "default").await;
    format!("http://{localstack_host}:31566")
}

/// Is there a "localstack" service in the "default" namespace.
async fn localstack_in_default_namespace(kube_client: &Client) -> bool {
    let service_api = Api::<Service>::namespaced(kube_client.clone(), "default");
    service_api.get("localstack").await.is_ok()
}

#[fixture]
pub async fn sqs_test_resources(#[future] kube_client: Client) -> SqsTestResources {
    let kube_client = kube_client.await;
    let mut guards = Vec::new();
    let endpoint_url = if localstack_in_default_namespace(&kube_client).await {
        println!("localstack detected, using localstack for SQS");
        patch_operator_for_localstack(&kube_client, &mut guards).await;
        Some(localstack_endpoint_external_url(&kube_client).await)
    } else {
        None
    };
    let sqs_client = get_sqs_client(endpoint_url).await;
    let queue1 = random_name_sqs_queue_with_echo_queue(false, &sqs_client, &mut guards).await;
    let queue2 = random_name_sqs_queue_with_echo_queue(true, &sqs_client, &mut guards).await;
    let k8s_service = sqs_consumer_service(&kube_client, &queue1, &queue2).await;
    SqsTestResources {
        k8s_service,
        queue1,
        queue2,
        sqs_client,
        _guards: guards,
    }
}

pub async fn write_sqs_messages(
    sqs_client: &aws_sdk_sqs::Client,
    queue: &QueueInfo,
    message_attribute_name: &str,
    message_attributes: &[&str],
    messages: &[&str],
) {
    let fifo = queue.name.ends_with(".fifo");
    let group_id = fifo.then_some("e2e-tests".to_string());
    for (i, (body, attr)) in messages.iter().zip(message_attributes).enumerate() {
        sqs_client
            .send_message()
            .queue_url(&queue.url)
            .message_body(*body)
            .message_attributes(
                message_attribute_name,
                MessageAttributeValue::builder()
                    .string_value(*attr)
                    .data_type("String")
                    .build()
                    .unwrap(),
            )
            .set_message_group_id(group_id.clone())
            .set_message_deduplication_id(fifo.then_some(i.to_string()))
            .send()
            .await
            .expect("Sending SQS message failed.");
    }
}
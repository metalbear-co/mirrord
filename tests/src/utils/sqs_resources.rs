#![cfg(test)]
#![cfg(feature = "operator")]

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    ops::Not,
    time::Instant,
};

use aws_credential_types::provider::{
    future::ProvideCredentials as ProvideCredentialsFuture, ProvideCredentials,
};
use aws_sdk_sqs::{
    operation::receive_message::ReceiveMessageOutput,
    types::{
        Message, MessageAttributeValue,
        QueueAttributeName::{FifoQueue, MessageRetentionPeriod},
    },
};
use futures_util::FutureExt;
use json_patch::{PatchOperation, ReplaceOperation, TestOperation};
use jsonptr::PointerBuf;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{EnvVar, Pod, Service},
};
use kube::{
    api::{Patch, PatchParams, PostParams},
    runtime::watcher::Config,
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

use super::{port_forwarder::PortForwarder, watch::Watcher};
use crate::utils::{kube_client, service_with_env, KubeService, ResourceGuard};

/// Name of an environment variable used by ghcr.io/metalbear-co/mirrord-sqs-forwarder.
///
/// The variable holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// Name of an environment variable used by ghcr.io/metalbear-co/mirrord-sqs-forwarder.
///
/// The variable holds the name of the second SQS queue to read from.
const QUEUE_NAME_ENV_VAR2: &str = "SQS_TEST_Q_NAME2";

/// Name of an environment variable used by ghcr.io/metalbear-co/mirrord-sqs-forwarder.
///
/// The variable holds the name of the SQS queue where the application sends messages read from
/// [`QUEUE_NAME_ENV_VAR1`].
const ECHO_QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_ECHO_Q_NAME1";

/// Name of an environment variable used by ghcr.io/metalbear-co/mirrord-sqs-forwarder.
///
/// The variable holds the name of the SQS queue where the application sends messages read from
/// [`QUEUE_NAME_ENV_VAR2`].
const ECHO_QUEUE_NAME_ENV_VAR2: &str = "SQS_TEST_ECHO_Q_NAME2";

/// Endpoint of localstack that should be accessible from within the cluster.
const LOCALSTACK_ENDPOINT_URL: &str = "http://localstack.default.svc.cluster.local:4566";

/// An SQS queue created for E2E tests.
pub struct SqsTestQueue {
    name: String,
    url: String,
    is_fifo: bool,
    client: aws_sdk_sqs::Client,
    _guard: ResourceGuard,
}

impl SqsTestQueue {
    const FIFO_GROUP_ID: &str = "e2e-tests";

    pub async fn new(name: String, fifo: bool, client: aws_sdk_sqs::Client) -> Self {
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
        let client_cloned = client.clone();

        let deleter = Some(
            async move {
                println!("Deleting SQS queue {queue_name}");
                let result = client_cloned
                    .delete_queue()
                    .queue_url(queue_url)
                    .send()
                    .await;
                if let Err(err) = result {
                    eprintln!("Could not delete SQS queue {queue_name}: {err}");
                }
            }
            .boxed(),
        );
        let guard = ResourceGuard {
            delete_on_fail: true,
            deleter,
        };

        Self {
            name,
            url,
            is_fifo: fifo,
            client,
            _guard: guard,
        }
    }

    /// Sends a message to this queue with the given body and attribute.
    pub async fn send_message(&self, body: &str, attribute: &str, attribute_value: &str) {
        println!(
            "Sending messages `{body}` with `{attribute}={attribute_value}` to queue {}.",
            self.name
        );

        let group_id = self.is_fifo.then(|| Self::FIFO_GROUP_ID.to_string());
        let message_dedup_id = self.is_fifo.then(|| rand::random::<u32>().to_string());

        self.client
            .send_message()
            .queue_url(&self.url)
            .message_body(body)
            .message_attributes(
                attribute,
                MessageAttributeValue::builder()
                    .string_value(attribute_value)
                    .data_type("String")
                    .build()
                    .unwrap(),
            )
            .set_message_group_id(group_id)
            .set_message_deduplication_id(message_dedup_id)
            .send()
            .await
            .expect("sending SQS message failed");
    }

    async fn delete_message(&self, receipt_handle: &str) {
        self.client
            .delete_message()
            .queue_url(&self.url)
            .receipt_handle(receipt_handle)
            .send()
            .await
            .expect("failed to delete SQS message");
    }

    /// Reads n messages from this queue and compares their bodies to the given data.
    ///
    /// Does not verify the order of messages.
    /// If you want to verify the order, call this function multiple times.
    pub async fn expect_messages(&self, mut expected_messages: Vec<&str>) {
        println!(
            "Verifying that {} next message(s) in queue {} is/are: {:?}",
            expected_messages.len(),
            self.name,
            expected_messages
        );

        while expected_messages.is_empty().not() {
            let max_messages = expected_messages.len().clamp(1, 10) as i32; // 1..=10 is the valid range for max returned messages

            let ReceiveMessageOutput { messages, .. } = self
                .client
                .receive_message()
                .queue_url(&self.url)
                .visibility_timeout(15)
                .wait_time_seconds(0)
                .max_number_of_messages(max_messages)
                .send()
                .await
                .expect("failed to receive message");

            let messages = messages.unwrap_or_default();
            assert!(
                messages.is_empty().not(),
                "some expected messages where not found in the queue: {expected_messages:?}"
            );

            for message in messages {
                let Message {
                    receipt_handle,
                    body,
                    ..
                } = message;
                let message = body.unwrap_or_default();
                println!("got message `{message}` from queue `{}`.", self.name);

                let position = expected_messages
                    .iter()
                    .position(|expected| *expected == message)
                    .expect("received an unexpected message");
                expected_messages.remove(position);

                self.delete_message(&receipt_handle.expect("no receipt handle"))
                    .await;
            }
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// K8s resources and SQS queues will be deleted when this is dropped.
pub struct SqsTestResources {
    k8s_service: KubeService,

    pub queue1: SqsTestQueue,
    pub echo_queue1: SqsTestQueue,
    pub queue2: SqsTestQueue,
    pub echo_queue2: SqsTestQueue,
    pub sqs_client: aws_sdk_sqs::Client,

    _localstack_portforwarder: Option<PortForwarder>,
    _operator_patch_guard: Option<ResourceGuard>,
}

impl SqsTestResources {
    pub fn namespace(&self) -> &str {
        self.k8s_service.namespace.as_str()
    }

    pub fn deployment_target(&self) -> String {
        self.k8s_service.deployment_target()
    }
}

/// A credential provider that makes the SQS SDK use localstack.
#[derive(Debug)]
struct LocalstackTestCredentialsProvider;

impl ProvideCredentials for LocalstackTestCredentialsProvider {
    fn provide_credentials<'a>(
        &'a self,
    ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        ProvideCredentialsFuture::ready(Ok(aws_credential_types::Credentials::new(
            "test", "test", None, None, "E2E",
        )))
    }
}

/// Get an SQS client. If a localstack URL is provided the client will use that localstack service.
async fn get_sqs_client(localstack_url: Option<String>) -> aws_sdk_sqs::Client {
    let mut config = aws_config::from_env();
    if let Some(endpoint_url) = localstack_url {
        println!("Using endpoint URL {endpoint_url}");
        config = config
            .endpoint_url(endpoint_url)
            .credentials_provider(LocalstackTestCredentialsProvider)
            .region(aws_types::region::Region::new("us-east-1"));
    }
    let config = config.load().await;
    aws_sdk_sqs::Client::new(&config)
}

/// Create the [`MirrordWorkloadQueueRegistry`] resource.
/// No [`ResourceGuard`] is needed here, because the whole namespace is guarded.
async fn create_queue_registry_resource(
    kube_client: &Client,
    namespace: &str,
    deployment_name: &str,
) {
    let name = format!("e2e-workload-queue-registry-{:x}", rand::random::<u16>());
    println!(
        "Creating {} {namespace}/{name}",
        MirrordWorkloadQueueRegistry::kind(&())
    );

    let api: Api<MirrordWorkloadQueueRegistry> = Api::namespaced(kube_client.clone(), namespace);
    let resource = MirrordWorkloadQueueRegistry::new(
        &name,
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

    api.create(&PostParams::default(), &resource)
        .await
        .expect("failed to create workload queue registry");

    println!(
        "Created {} {namespace}/{name}",
        MirrordWorkloadQueueRegistry::kind(&())
    );
}

/// Create a microservice for the sqs test application.
/// That service will be configured to use the "localstack" service in the "default" namespace for
/// all AWS stuff.
async fn sqs_consumer_service(
    kube_client: &Client,
    queue1: &SqsTestQueue,
    queue2: &SqsTestQueue,
    echo_queue1: &SqsTestQueue,
    echo_queue2: &SqsTestQueue,
) -> KubeService {
    let namespace = format!("e2e-tests-sqs-splitting-{}", crate::utils::random_string());
    service_with_env(
        &namespace,
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-sqs-forwarder:latest",
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
              "name": ECHO_QUEUE_NAME_ENV_VAR1,
              "value": echo_queue1.name.as_str()
            },
            {
              "name": ECHO_QUEUE_NAME_ENV_VAR2,
              "value": echo_queue2.name.as_str()
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
              "value": "test"
            },
            {
              "name": "AWS_ACCESS_KEY_ID",
              "value": "test"
            },
            {
              "name": "AWS_SECRET_KEY",
              "value": "test"
            },
        ]),
    )
    .await
}

const LOCALSTACK_OPERATOR_ENV: [(&str, &str); 5] = [
    ("AWS_ENDPOINT_URL", LOCALSTACK_ENDPOINT_URL),
    ("AWS_REGION", "us-east-1"),
    ("AWS_SECRET_ACCESS_KEY", "test"),
    ("AWS_ACCESS_KEY_ID", "test"),
    ("AWS_SECRET_KEY", "test"),
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
///
/// Return a [`ResourceGuard`] that will restore the original env when dropped.
async fn patch_operator_for_localstack(client: &Client) -> ResourceGuard {
    let api = Api::<Deployment>::namespaced(client.clone(), "mirrord");

    let operator_deploy = api
        .get(OPERATOR_NAME)
        .await
        .expect("failed to find the operator deployment in the mirrord namespace");
    let selector = operator_deploy
        .spec
        .as_ref()
        .unwrap()
        .selector
        .match_labels
        .as_ref()
        .unwrap()
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");

    let (container_index, original_env) = take_env(operator_deploy);

    let mut new_env = original_env.clone();
    let aws_env = HashMap::from(LOCALSTACK_OPERATOR_ENV);

    new_env.retain(|env| aws_env.contains_key(env.name.as_str()).not());
    for (name, value) in aws_env {
        new_env.push(EnvVar {
            name: name.to_string(),
            value: Some(value.to_string()),
            value_from: None,
        })
    }

    let container_index = container_index.to_string();
    let mut path = PointerBuf::from_tokens(["spec", "template", "spec", "containers"]);
    path.push_back(container_index);
    path.push_back("env");
    let value = serde_json::to_value(&new_env).unwrap();

    api.patch(
        OPERATOR_NAME,
        &PatchParams::default(),
        &Patch::<Deployment>::Json(json_patch::Patch(vec![PatchOperation::Replace(
            ReplaceOperation {
                path: path.clone(),
                value: value.clone(),
            },
        )])),
    )
    .await
    .unwrap();

    let api = api.clone();
    let restore_env = async move {
        println!("Restoring AWS env vars from before the SQS test");
        let _ = api
            .patch(
                OPERATOR_NAME,
                &PatchParams::default(),
                &Patch::<Deployment>::Json(json_patch::Patch(vec![
                    PatchOperation::Test(TestOperation {
                        path: path.clone(),
                        value: value.clone(),
                    }),
                    PatchOperation::Replace(json_patch::ReplaceOperation {
                        path: path.clone(),
                        value: serde_json::to_value(&original_env).unwrap(),
                    }),
                ])),
            )
            .await
            .inspect_err(|error| {
                println!("Failed to patch operator deployment to restore AWS env: {error}");
            });
    };

    let guard = ResourceGuard {
        delete_on_fail: true,
        deleter: Some(restore_env.boxed()),
    };

    let start = Instant::now();
    // Wait until:
    // 1. At least one operator pod is ready
    // 2. All ready operator pods use the new env
    Watcher::new(
        Api::<Pod>::namespaced(client.clone(), "mirrord"),
        Config {
            label_selector: Some(selector),
            ..Default::default()
        },
        move |pods| {
            let patched_operator_pods = pods
                .values()
                .filter_map(|pod| {
                    let status = pod.status.as_ref()?;
                    if status.phase.as_deref()? != "Running" {
                        return None;
                    }

                    let all_containers_ready = status
                        .container_statuses
                        .as_ref()?
                        .iter()
                        .all(|status| status.ready);
                    if !all_containers_ready {
                        return None;
                    }

                    pod.spec
                        .as_ref()?
                        .containers
                        .iter()
                        .find(|c| c.name == OPERATOR_NAME)?
                        .env
                        .as_ref()
                })
                .filter(|env| *env == &new_env)
                .count();

            patched_operator_pods == pods.len() && patched_operator_pods > 0
        },
    );
    let elapsed = start.elapsed();
    println!(
        "mirrord Operator was ready again after {}s",
        elapsed.as_secs_f32()
    );

    guard
}

/// - Check if there is a localstack instance deployed in the default namespace and patch the
///   mirrord operator with SQS env to use that localstack instance.
/// - Create an SQS client (that uses localstack if there).
/// - Create 4 guarded SQS queues with partially random names: 2 queues for the test applications to
///   consume from, and two "echo" queues from the deployed test application to forward messages to,
///   so that the test can verify it received them.
/// - Deploy a consumer service in a new guarded namespace with a partially random name.
/// - Create a [`MirrordWorkloadQueueRegistry`] resource in the test's namespace.
#[fixture]
pub async fn sqs_test_resources(#[future] kube_client: Client) -> SqsTestResources {
    let kube_client = kube_client.await;

    let localstack_setup = {
        let localstack = Api::<Service>::namespaced(kube_client.clone(), "default")
            .get("localstack")
            .await
            .ok();

        if let Some(localstack) = localstack {
            println!("localstack detected, using localstack for SQS");

            let portforwarder =
                PortForwarder::new_for_service(kube_client.clone(), &localstack, 4566).await;
            let guard = patch_operator_for_localstack(&kube_client).await;

            Some((portforwarder, guard))
        } else {
            None
        }
    };
    let (localstack_portforwarder, operator_patch_guard) = localstack_setup.unzip();

    let endpoint_url = localstack_portforwarder
        .as_ref()
        .map(|addr| format!("http://{}", addr.address()));
    let sqs_client = get_sqs_client(endpoint_url).await;

    let name1 = format!("test-queue-{:x}", rand::random::<u16>());
    let echo_name1 = format!("echo-{name1}");
    let queue1 = SqsTestQueue::new(name1, false, sqs_client.clone()).await;
    let echo_queue1 = SqsTestQueue::new(echo_name1, false, sqs_client.clone()).await;

    let name2 = format!("test-queue-{:x}.fifo", rand::random::<u16>());
    let echo_name2 = format!("echo-{name2}");
    let queue2 = SqsTestQueue::new(name2, true, sqs_client.clone()).await;
    let echo_queue2 = SqsTestQueue::new(echo_name2, true, sqs_client.clone()).await;

    let k8s_service =
        sqs_consumer_service(&kube_client, &queue1, &queue2, &echo_queue1, &echo_queue2).await;

    create_queue_registry_resource(&kube_client, &k8s_service.namespace, &k8s_service.name).await;

    SqsTestResources {
        k8s_service,
        queue1,
        echo_queue1,
        queue2,
        echo_queue2,
        sqs_client,
        _localstack_portforwarder: localstack_portforwarder,
        _operator_patch_guard: operator_patch_guard,
    }
}

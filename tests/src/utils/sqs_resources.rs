#![cfg(test)]
#![cfg(feature = "operator")]

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    time::Duration,
};

use aws_config::Region;
use aws_credential_types::provider::{
    future::ProvideCredentials as ProvideCredentialsFuture, ProvideCredentials,
};
use aws_sdk_sqs::types::{
    MessageAttributeValue,
    QueueAttributeName::{FifoQueue, MessageRetentionPeriod},
};
use futures_util::FutureExt;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{EnvVar, Service},
};
use kube::{api::PostParams, runtime::wait::await_condition, Api, Client};
use mirrord_operator::{
    crd::{
        is_session_ready, MirrordSqsSession, MirrordWorkloadQueueRegistry,
        MirrordWorkloadQueueRegistrySpec, QueueConsumer, QueueConsumerType, QueueNameSource,
        SplitQueue, SqsQueueDetails,
    },
    setup::OPERATOR_NAME,
};

use super::{port_forwarder::PortForwarder, watch::Watcher};
use crate::utils::{
    kube_service::KubeService, resource_guard::ResourceGuard, services::service_with_env,
};

/// Name of the environment variable that holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue to read from.
const QUEUE2_URL_ENV_VAR: &str = "SQS_TEST_Q2_URL";

/// Regex pattern that matches both [`QUEUE_NAME_ENV_VAR1`] and [`QUEUE2_URL_ENV_VAR`].
const BOTH_QUEUES_REGEX_PATTERN: &str = "SQS_TEST_Q.+";

/// Name of the environment variable that holds the name of the first SQS queue the deployed
/// application will write to.
const ECHO_QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_ECHO_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue the deployed
/// application will write to.
const ECHO_QUEUE_NAME_ENV_VAR2: &str = "SQS_TEST_ECHO_Q_NAME2";

/// Does not have to be dynamic because each test run creates its own namespace.
const QUEUE_REGISTRY_RESOURCE_NAME: &str = "mirrord-e2e-test-queue-registry";

pub struct QueueInfo {
    pub name: String,
    pub url: String,
}

/// K8s resources will be deleted when this is dropped.
pub struct SqsTestResources {
    pub kube_client: Client,
    k8s_service: KubeService,
    pub queue1: QueueInfo,
    pub echo_queue1: QueueInfo,
    pub queue2: QueueInfo,
    pub echo_queue2: QueueInfo,
    pub sqs_client: aws_sdk_sqs::Client,
    _guards: Vec<ResourceGuard>,
    /// Keeps portforwarding to the localstack service alive.
    ///
    /// [`None`] if we're not using localstack.
    _localstack_portforwarder: Option<PortForwarder>,
}

impl SqsTestResources {
    pub fn namespace(&self) -> &str {
        self.k8s_service.namespace.as_str()
    }

    pub fn deployment_target(&self) -> String {
        self.k8s_service.deployment_target()
    }

    /// Count the temp queues created by the SQS-operator for this test instance.
    pub async fn count_temp_queues(&self) -> usize {
        self.sqs_client
            .list_queues()
            .queue_name_prefix("mirrord-")
            .send()
            .await
            .unwrap()
            .queue_urls
            .unwrap_or_default()
            .iter()
            .filter(|q_url| q_url.contains(&self.queue1.name) || q_url.contains(&self.queue2.name))
            .count()
    }

    /// Wait for all the temp queues created by the SQS operator for queues from this test instance
    /// to be deleted.
    /// Waiting for `secs` seconds.
    pub async fn wait_for_temp_queue_deletion(&self) {
        loop {
            if self.count_temp_queues().await == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

const AWS_ENDPOINT_ENV: &str = "AWS_ENDPOINT_URL";

/// Required to build an SQS client.
const AWS_CREDS_ENVS: &[&'static str] = &[
    AWS_ENDPOINT_ENV,
    "AWS_REGION",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_KEY",
];

/// A credential provider that makes the SQS SDK use localstack.
#[derive(Debug)]
struct TestCredentialsProvider {
    secret_access_key: String,
    access_key_id: String,
}

impl ProvideCredentials for TestCredentialsProvider {
    fn provide_credentials<'a>(
        &'a self,
    ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        ProvideCredentialsFuture::ready(Ok(aws_credential_types::Credentials::new(
            &self.access_key_id,
            &self.secret_access_key,
            None,
            None,
            "E2E",
        )))
    }
}

/// Get an SQS client. If a localstack URL is provided the client will use that localstack service.
async fn get_sqs_client(mut env: HashMap<String, String>) -> aws_sdk_sqs::Client {
    let config = aws_config::from_env()
        .empty_test_environment()
        .region(Region::new(env.remove("AWS_REGION").unwrap()))
        .endpoint_url(env.remove(AWS_ENDPOINT_ENV).unwrap())
        .credentials_provider(TestCredentialsProvider {
            secret_access_key: env.remove("AWS_SECRET_ACCESS_KEY").unwrap(),
            access_key_id: env.remove("AWS_ACCESS_KEY_ID").unwrap(),
        });

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
) -> (QueueInfo, QueueInfo) {
    let q_name = format!(
        "E2ETest-{}{}",
        crate::utils::random_string(),
        if fifo { ".fifo" } else { "" }
    );

    let echo_q_name = format!("Echo{q_name}");
    // Create queue and resource guard

    (
        sqs_queue(fifo, client, guards, q_name).await,
        sqs_queue(fifo, client, guards, echo_q_name).await,
    )
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

/// Create the `MirrordWorkloadQueueRegistry` K8s resource.
/// No ResourceGuard needed, the namespace is guarded.
pub async fn create_queue_registry_resource(
    kube_client: &Client,
    namespace: &str,
    deployment_name: &str,
    use_regex: bool,
) {
    println!("Creating MirrordWorkloadQueueRegistry resource");

    let queues = if use_regex {
        BTreeMap::from([(
            "e2e-test-queues".to_string(),
            SplitQueue::Sqs(SqsQueueDetails {
                name_source: QueueNameSource::RegexPattern(BOTH_QUEUES_REGEX_PATTERN.to_string()),
                tags: None,
                fallback_name: None,
                names_from_json_map: None,
            }),
        )])
    } else {
        BTreeMap::from([
            (
                "e2e-test-queue1".to_string(),
                SplitQueue::Sqs(SqsQueueDetails {
                    name_source: QueueNameSource::EnvVar(QUEUE_NAME_ENV_VAR1.to_string()),
                    tags: None,
                    fallback_name: None,
                    names_from_json_map: None,
                }),
            ),
            (
                "e2e-test-queue2".to_string(),
                SplitQueue::Sqs(SqsQueueDetails {
                    name_source: QueueNameSource::EnvVar(QUEUE2_URL_ENV_VAR.to_string()),
                    tags: None,
                    fallback_name: None,
                    names_from_json_map: None,
                }),
            ),
        ])
    };

    let qr_api: Api<MirrordWorkloadQueueRegistry> = Api::namespaced(kube_client.clone(), namespace);
    let queue_registry = MirrordWorkloadQueueRegistry::new(
        QUEUE_REGISTRY_RESOURCE_NAME,
        MirrordWorkloadQueueRegistrySpec {
            queues,
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
    println!("MirrordWorkloadQueueRegistry resource created at namespace {namespace} with name {QUEUE_REGISTRY_RESOURCE_NAME}");
}

/// Create a microservice for the sqs test application.
/// That service will be configured to use the "localstack" service in the "default" namespace for
/// all AWS stuff.
async fn sqs_consumer_service(
    kube_client: &Client,
    queue1: &QueueInfo,
    queue2: &QueueInfo,
    echo_queue1: &QueueInfo,
    echo_queue2: &QueueInfo,
    aws_creds: HashMap<String, String>,
) -> KubeService {
    let namespace = format!("e2e-tests-sqs-splitting-{}", crate::utils::random_string());

    let env = [
        (QUEUE_NAME_ENV_VAR1.into(), queue1.name.clone()),
        (QUEUE2_URL_ENV_VAR.into(), queue2.url.clone()),
        (ECHO_QUEUE_NAME_ENV_VAR1.into(), echo_queue1.name.clone()),
        (ECHO_QUEUE_NAME_ENV_VAR2.into(), echo_queue2.name.clone()),
    ]
    .into_iter()
    .chain(aws_creds)
    .map(|(name, value)| EnvVar {
        name,
        value: Some(value),
        value_from: None,
    })
    .collect::<Vec<_>>();

    service_with_env(
        &namespace,
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-sqs-forwarder:latest",
        "queue-forwarder",
        false,
        kube_client.clone(),
        serde_json::to_value(env).unwrap(),
    )
    .await
}

/// Attempts to find the `localstack` service in the `localstack` namespace.
async fn get_localstack_service(kube_client: &Client) -> Option<Service> {
    let service_api = Api::<Service>::namespaced(kube_client.clone(), "localstack");
    service_api.get("localstack").await.ok()
}

/// Watch [`MirrordSqsSession`] resources in the given namespace and return once two unique sessions
/// are ready.
///
/// Return `Err(())` if there was an error while watching or if a session was deleted before 2 were
/// ready.
pub async fn watch_sqs_sessions(kube_client: Client, namespace: &str) {
    let api = Api::<MirrordSqsSession>::namespaced(kube_client, namespace);
    Watcher::new(api, Default::default(), |sessions| {
        sessions
            .values()
            .filter(|s| is_session_ready(Some(s)))
            .count()
            == 2
    })
    .run()
    .await;
}

pub async fn await_registry_status(kube_client: Client, namespace: &str) {
    let api: Api<MirrordWorkloadQueueRegistry> = Api::namespaced(kube_client, namespace);
    let has_status =
        |qr: Option<&MirrordWorkloadQueueRegistry>| qr.is_some_and(|qr| qr.status.is_some());

    await_condition(api, QUEUE_REGISTRY_RESOURCE_NAME, has_status)
        .await
        .unwrap();
}

/// - Fetch AWS credentials from the operator container.
/// - Create an SQS client (that uses localstack if there).
/// - Create 4 guarded SQS queues with partially random names: 2 queues for the test applications to
///   consume from, and two "echo" queues from the deployed test application to forward messages to,
///   so that the test can verify it received them.
/// - Start a task that waits for 2 SQS Sessions to be ready.
/// - Deploy a consumer service in a new guarded namespace with a partially random name.
/// - Create a `MirrordWorkloadQueueRegistry` resource in the test's namespace.
pub async fn sqs_test_resources(kube_client: Client, use_regex: bool) -> SqsTestResources {
    let mut guards = Vec::new();

    let mut aws_creds = fetch_aws_creds(kube_client.clone()).await;

    let aws_endpoint_url = aws_creds.get_mut(AWS_ENDPOINT_ENV).unwrap();
    let localstack_portforwarder = if aws_endpoint_url.contains("localstack.svc.cluster") {
        let localstack = get_localstack_service(&kube_client).await.unwrap();
        let localstack_portforwarder =
            PortForwarder::new_for_service(kube_client.clone(), &localstack, 4566).await;
        *aws_endpoint_url = format!("http://localhost:{}", localstack_portforwarder.address());
        Some(localstack_portforwarder)
    } else {
        None
    };

    let sqs_client = get_sqs_client(aws_creds.clone()).await;

    let (queue1, echo_queue1) =
        random_name_sqs_queue_with_echo_queue(false, &sqs_client, &mut guards).await;
    let (queue2, echo_queue2) =
        random_name_sqs_queue_with_echo_queue(true, &sqs_client, &mut guards).await;
    let k8s_service = sqs_consumer_service(
        &kube_client,
        &queue1,
        &queue2,
        &echo_queue1,
        &echo_queue2,
        aws_creds,
    )
    .await;

    create_queue_registry_resource(
        &kube_client,
        &k8s_service.namespace,
        &k8s_service.name,
        use_regex,
    )
    .await;

    SqsTestResources {
        kube_client,
        k8s_service,
        queue1,
        echo_queue1,
        queue2,
        echo_queue2,
        sqs_client,
        _guards: guards,
        _localstack_portforwarder: localstack_portforwarder,
    }
}

pub async fn write_sqs_messages(
    sqs_client: &aws_sdk_sqs::Client,
    queue: &QueueInfo,
    message_attribute_name: &str,
    message_attributes: &[&str],
    messages: &[&str],
) {
    println!("Sending messages {messages:?} to queue {}.", queue.name);
    let fifo = queue.name.ends_with(".fifo");
    let group_id = fifo.then_some("e2e-tests".to_string());
    for (i, (body, attr)) in messages.iter().zip(message_attributes).enumerate() {
        println!(
            r#"Sending message "{body}" with attribute "{message_attribute_name}: {attr}" to queue {}"#,
            queue.name
        );
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

/// Fetches AWS client environment from the operator container spec.
async fn fetch_aws_creds(client: Client) -> HashMap<String, String> {
    let env_map = Api::<Deployment>::namespaced(client, "mirrord")
        .get(OPERATOR_NAME)
        .await
        .unwrap()
        .spec
        .unwrap()
        .template
        .spec
        .unwrap()
        .containers
        .into_iter()
        .find(|container| container.name == OPERATOR_NAME)
        .unwrap()
        .env
        .unwrap()
        .into_iter()
        .filter_map(|env| {
            if AWS_CREDS_ENVS.contains(&env.name.as_str()) {
                Some((env.name, env.value.unwrap()))
            } else {
                None
            }
        })
        .collect::<HashMap<_, _>>();

    assert_eq!(
        env_map.len(),
        AWS_CREDS_ENVS.len(),
        "operator is not properly configured for SQS splitting tests, env=[{env_map:?}], expected={AWS_CREDS_ENVS:?}",
    );

    env_map
}

#![cfg(test)]
#![cfg(feature = "operator")]

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    ops::Not,
    time::Duration,
};

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
    core::v1::{EnvVar, Pod, Service},
};
use kube::{
    api::{Patch, PatchParams, PostParams},
    runtime::{wait::await_condition, watcher},
    Api, Client, Resource,
};
use mirrord_operator::{
    crd::{
        is_session_ready, MirrordSqsSession, MirrordWorkloadQueueRegistry,
        MirrordWorkloadQueueRegistrySpec, QueueConsumer, QueueConsumerType, QueueNameSource,
        SplitQueue, SqsQueueDetails,
    },
    setup::OPERATOR_NAME,
};
use rstest::fixture;

use super::{port_forwarder::PortForwarder, watch::Watcher};
use crate::utils::{
    kube_client, kube_service::KubeService, resource_guard::ResourceGuard,
    services::service_with_env,
};

/// Name of the environment variable that holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue to read from.
const QUEUE2_URL_ENV_VAR: &str = "SQS_TEST_Q2_URL";

/// Name of the environment variable that holds the name of the first SQS queue the deployed
/// application will write to.
const ECHO_QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_ECHO_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue the deployed
/// application will write to.
const ECHO_QUEUE_NAME_ENV_VAR2: &str = "SQS_TEST_ECHO_Q_NAME2";

/// Does not have to be dynamic because each test run creates its own namespace.
const QUEUE_REGISTRY_RESOURCE_NAME: &str = "mirrord-e2e-test-queue-registry";

/// To mark the operator deployment is currently patched to use localstack.
const LOCALSTACK_PATCH_LABEL_NAME: &str = "sqs-e2e-tests-localstack-patch";

const LOCALSTACK_ENDPOINT_URL: &str = "http://localstack.localstack.svc.cluster.local:4566";

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
    pub async fn wait_for_temp_queue_deletion(&self, secs: u64) {
        tokio::time::timeout(Duration::from_secs(secs), async {
            loop {
                if self.count_temp_queues().await == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        })
        .await
        .expect("temp SQS queues were not deleted in time after clients exited.");
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
            .region(aws_types::region::Region::new("eu-north-1"));
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
) {
    println!("Creating MirrordWorkloadQueueRegistry resource");
    let qr_api: Api<MirrordWorkloadQueueRegistry> = Api::namespaced(kube_client.clone(), namespace);
    let queue_registry = MirrordWorkloadQueueRegistry::new(
        QUEUE_REGISTRY_RESOURCE_NAME,
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
                        name_source: QueueNameSource::EnvVar(QUEUE2_URL_ENV_VAR.to_string()),
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
              "name": QUEUE2_URL_ENV_VAR,
              "value": queue2.url.as_str()
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
              "value": "eu-north-1"
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

const AWS_ENV: [(&str, &str); 5] = [
    ("AWS_ENDPOINT_URL", LOCALSTACK_ENDPOINT_URL),
    ("AWS_REGION", "eu-north-1"),
    ("AWS_SECRET_ACCESS_KEY", "test"),
    ("AWS_ACCESS_KEY_ID", "test"),
    ("AWS_SECRET_KEY", "test"),
];

/// Take env from deployment, return container index and env
fn take_env(deployment: &mut Deployment) -> (usize, Vec<EnvVar>) {
    let (index, container) = deployment
        .spec
        .as_mut()
        .unwrap()
        .template
        .spec
        .as_mut()
        .unwrap()
        .containers
        .iter_mut()
        .enumerate()
        .find(|(_, container)| container.name == OPERATOR_NAME)
        .expect("could not find right operator container");

    (index, container.env.take().unwrap_or_default())
}

/// Patch the deployed operator to use localstack for all AWS API calls.
/// Return a ResourceGuard that will restore the original env when dropped.
async fn patch_operator_for_localstack(client: &Client, guards: &mut Vec<ResourceGuard>) {
    let deploy_api = Api::<Deployment>::namespaced(client.clone(), "mirrord");

    let mut operator_deploy = deploy_api
        .get(OPERATOR_NAME)
        .await
        .expect(r#"Did not find the operator's deployment in the "mirrord" namespace."#);

    if let Some(label_map) = operator_deploy.meta().labels.as_ref() {
        if label_map.contains_key(LOCALSTACK_PATCH_LABEL_NAME) {
            println!("operator already patched for localstack, not patching.");
            return;
        }
    };

    let (container_index, mut env) = take_env(&mut operator_deploy);

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
    let path = jsonptr::PointerBuf::from_tokens([
        "spec",
        "template",
        "spec",
        "containers",
        container_index.as_str(),
        "env",
    ]);
    let env_path = path.clone();
    let value = serde_json::to_value(&env).unwrap();

    let label_path =
        jsonptr::PointerBuf::from_tokens(["metadata", "labels", LOCALSTACK_PATCH_LABEL_NAME]);

    let operator_deploy = deploy_api
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

    let container_name_path = jsonptr::PointerBuf::from_tokens([
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

    let namespace = operator_deploy.metadata.namespace.as_deref().unwrap();
    let selector = operator_deploy
        .spec
        .as_ref()
        .unwrap()
        .selector
        .match_labels
        .as_ref()
        .unwrap();

    wait_for_operator_patch(
        client.clone(),
        env,
        namespace,
        selector,
        Duration::from_secs(120),
    )
    .await;
}

/// Waits until the operator container uses the expected env and is ready.
async fn wait_for_operator_patch(
    client: Client,
    expected_env: Vec<EnvVar>,
    namespace: &str,
    selector: &BTreeMap<String, String>,
    timeout: Duration,
) {
    let label_selector = selector
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    let config = watcher::Config {
        label_selector: Some(label_selector),
        ..Default::default()
    };

    let condition = move |pods: &HashMap<String, Pod>| {
        if pods.is_empty() {
            return false;
        }

        let ready = pods
            .values()
            .filter_map(|pod| {
                let status = pod.status.as_ref()?;
                let phase = status.phase.as_deref()?;
                if phase != "Running" {
                    return None;
                }

                if status
                    .container_statuses
                    .as_ref()?
                    .iter()
                    .all(|c| c.ready)
                    .not()
                {
                    return None;
                }

                let env = pod
                    .spec
                    .as_ref()?
                    .containers
                    .iter()
                    .find(|c| c.name == OPERATOR_NAME)?
                    .env
                    .as_ref()?;
                if *env != expected_env {
                    return None;
                }

                Some(())
            })
            .count();

        ready == pods.len()
    };

    let mut watcher = Watcher::new(Api::<Pod>::namespaced(client, namespace), config, condition);
    tokio::time::timeout(timeout, watcher.run()).await.unwrap();
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

/// - Check if there is a localstack instance deployed in the default namespace and patch the
///   mirrord operator with SQS env to use that localstack instance.
/// - Create an SQS client (that uses localstack if there).
/// - Create 4 guarded SQS queues with partially random names: 2 queues for the test applications to
///   consume from, and two "echo" queues from the deployed test application to forward messages to,
///   so that the test can verify it received them.
/// - Start a task that waits for 2 SQS Sessions to be ready.
/// - Deploy a consumer service in a new guarded namespace with a partially random name.
/// - Create a `MirrordWorkloadQueueRegistry` resource in the test's namespace.
#[fixture]
pub async fn sqs_test_resources(#[future] kube_client: Client) -> SqsTestResources {
    let kube_client = kube_client.await;
    let mut guards = Vec::new();
    let localstack_portforwarder =
        if let Some(localstack) = get_localstack_service(&kube_client).await {
            println!("localstack detected, using localstack for SQS");
            patch_operator_for_localstack(&kube_client, &mut guards).await;
            Some(PortForwarder::new_for_service(kube_client.clone(), &localstack, 4566).await)
        } else {
            None
        };

    let localstack_url = localstack_portforwarder
        .as_ref()
        .map(PortForwarder::address)
        .map(|addr| format!("http://{addr}"));
    let sqs_client = get_sqs_client(localstack_url).await;
    let (queue1, echo_queue1) =
        random_name_sqs_queue_with_echo_queue(false, &sqs_client, &mut guards).await;
    let (queue2, echo_queue2) =
        random_name_sqs_queue_with_echo_queue(true, &sqs_client, &mut guards).await;
    let k8s_service =
        sqs_consumer_service(&kube_client, &queue1, &queue2, &echo_queue1, &echo_queue2).await;

    create_queue_registry_resource(&kube_client, &k8s_service.namespace, &k8s_service.name).await;

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

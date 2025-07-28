#![cfg(test)]
#![cfg(feature = "operator")]

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    ops::Not,
    time::Duration,
};

use aws_config::Region;
use aws_credential_types::provider::{
    future::ProvideCredentials as ProvideCredentialsFuture, ProvideCredentials,
};
use aws_sdk_sqs::types::{
    builders::SendMessageBatchRequestEntryBuilder,
    MessageAttributeValue,
    QueueAttributeName::{FifoQueue, MessageRetentionPeriod},
};
use futures_util::FutureExt;
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{
            ConfigMap, ConfigMapEnvSource, ConfigMapKeySelector, EnvFromSource, EnvVar,
            EnvVarSource, Namespace, Pod, Service,
        },
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{
    api::{Patch, PatchParams, PostParams},
    Api, Client, Resource,
};
use mirrord_kube::api::kubernetes::rollout::Rollout;
use mirrord_operator::{
    crd::{
        MirrordSqsSession, MirrordWorkloadQueueRegistry, MirrordWorkloadQueueRegistrySpec,
        QueueConsumer, QueueConsumerType, QueueNameSource, SplitQueue, SqsQueueDetails,
        SqsSessionStatus,
    },
    setup::OPERATOR_NAME,
};
use serde::Serialize;
use serde_json::Value;

use super::{get_test_resource_label_map, port_forwarder::PortForwarder, watch::Watcher};
use crate::utils::{
    cluster_resource::{
        argo_rollout_from_json, argo_rollout_with_template_from_json, deployment_from_json,
        service_from_json,
    },
    kube_service::KubeService,
    resource_guard::ResourceGuard,
    watch,
};

/// Name of the environment variable that holds the name of the first SQS queue to read from.
const QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue to read from.
const QUEUE2_URL_ENV_VAR: &str = "SQS_TEST_Q2_URL";

/// Regex pattern that matches both [`QUEUE_NAME_ENV_VAR1`] and [`QUEUE2_URL_ENV_VAR`].
const BOTH_QUEUES_REGEX_PATTERN: &str = "SQS_TEST_Q.+";

/// Name of the environment variable that holds a json object with the names/urls of the queues to
/// use. If this env var is set, the application will use the names from the json and will ignore
/// the two env vars.
const QUEUE_JSON_ENV_VAR: &str = "SQS_TEST_Q_JSON";

/// The keys inside the json object.
const QUEUE1_NAME_KEY: &str = "queue1_name";
const QUEUE2_URL_KEY: &str = "queue2_url";

/// Name of the environment variable that holds the name of the first SQS queue the deployed
/// application will write to.
const ECHO_QUEUE_NAME_ENV_VAR1: &str = "SQS_TEST_ECHO_Q_NAME1";

/// Name of the environment variable that holds the name of the second SQS queue the deployed
/// application will write to.
const ECHO_QUEUE_NAME_ENV_VAR2: &str = "SQS_TEST_ECHO_Q_NAME2";

/// Does not have to be dynamic because each test run creates its own namespace.
const QUEUE_REGISTRY_RESOURCE_NAME: &str = "mirrord-e2e-test-queue-registry";

#[derive(Clone, Copy)]
pub enum TestWorkload {
    Deployment,
    ArgoRolloutWithWorkloadRef,
    ArgoRolloutWithTemplate,
}

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

    pub fn target(&self) -> String {
        self.k8s_service.rollout_or_deployment_target()
    }

    /// Count the temp queues created by the SQS-operator for this test instance.
    pub async fn count_temp_queues(&self) -> usize {
        println!("Counting temporary queues created by the SQS operator for this test.");

        let urls = self
            .sqs_client
            .list_queues()
            .queue_name_prefix("mirrord-")
            .send()
            .await
            .unwrap()
            .queue_urls
            .unwrap_or_default();
        let count = urls
            .iter()
            .filter(|q_url| q_url.contains(&self.queue1.name) || q_url.contains(&self.queue2.name))
            .count();

        println!(
            "Found {count} queues. TEST_QUEUE_NAMES=[{}, {}], ALL_QUEUE_URLS={urls:?}.",
            self.queue1.name, self.queue2.name
        );
        count
    }

    /// Wait for all the temp queues created by the SQS operator
    /// for this test instance to be deleted.
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
const AWS_REGION_ENV: &str = "AWS_REGION";
const AWS_SECRET_ACCESS_KEY_ENV: &str = "AWS_SECRET_ACCESS_KEY";
const AWS_ACCESS_KEY_ID_ENV: &str = "AWS_ACCESS_KEY_ID";

/// Required to build an SQS client.
const AWS_CREDS_ENVS: &[&str] = &[
    AWS_ENDPOINT_ENV,
    AWS_REGION_ENV,
    AWS_SECRET_ACCESS_KEY_ENV,
    AWS_ACCESS_KEY_ID_ENV,
];

/// The name of the config map that will be used in an `envFrom` field.
const CONFIG_MAP_FOR_ENV_FROM: &str = "for-env-from";

/// The name of the config map that will be used in a `valueFrom` field.
const CONFIG_MAP_FOR_VALUE_FROM: &str = "for-value-from";

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
        .region(Region::new(env.remove(AWS_REGION_ENV).unwrap()))
        .endpoint_url(env.remove(AWS_ENDPOINT_ENV).unwrap())
        .credentials_provider(TestCredentialsProvider {
            secret_access_key: env.remove(AWS_SECRET_ACCESS_KEY_ENV).unwrap(),
            access_key_id: env.remove(AWS_ACCESS_KEY_ID_ENV).unwrap(),
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

/// Create the [`MirrordWorkloadQueueRegistry`] K8s resource.
/// No [`ResourceGuard`] is created, assuming that the whole namespace is guared.
///
/// # Arguments:
/// * `fallback_queues` - if not `None`, then a fallback will be set using the given queues.
pub async fn create_queue_registry_resource(
    kube_client: &Client,
    kube_service: &KubeService,
    use_regex: bool,
    fallback_queues: Option<(&QueueInfo, &QueueInfo)>,
) {
    println!("Creating MirrordWorkloadQueueRegistry resource");

    let queues = if let Some((queue1, queue2)) = fallback_queues {
        if use_regex {
            panic!(
                "Not implemented: creating a test queue registry with RegexPattern + json object"
            );
        }
        BTreeMap::from([(
            "e2e-test-queues".to_string(),
            SplitQueue::Sqs(SqsQueueDetails {
                name_source: QueueNameSource::EnvVar(QUEUE_JSON_ENV_VAR.to_string()),
                tags: None,
                fallback_name: Some(
                    serde_json::json!({
                        QUEUE1_NAME_KEY: &queue1.name,
                        QUEUE2_URL_KEY: &queue2.url
                    })
                    .to_string(),
                ),
                names_from_json_map: Some(true),
                sns: None,
            }),
        )])
    } else if use_regex {
        BTreeMap::from([(
            "e2e-test-queues".to_string(),
            SplitQueue::Sqs(SqsQueueDetails {
                name_source: QueueNameSource::RegexPattern(BOTH_QUEUES_REGEX_PATTERN.to_string()),
                tags: None,
                fallback_name: None,
                names_from_json_map: None,
                sns: None,
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
                    sns: None,
                }),
            ),
            (
                "e2e-test-queue2".to_string(),
                SplitQueue::Sqs(SqsQueueDetails {
                    name_source: QueueNameSource::EnvVar(QUEUE2_URL_ENV_VAR.to_string()),
                    tags: None,
                    fallback_name: None,
                    names_from_json_map: None,
                    sns: None,
                }),
            ),
        ])
    };

    let qr_api: Api<MirrordWorkloadQueueRegistry> =
        Api::namespaced(kube_client.clone(), &kube_service.namespace);
    let workload_type = if kube_service.rollout.is_some() {
        QueueConsumerType::Rollout
    } else {
        QueueConsumerType::Deployment
    };
    let queue_registry = MirrordWorkloadQueueRegistry::new(
        QUEUE_REGISTRY_RESOURCE_NAME,
        MirrordWorkloadQueueRegistrySpec {
            queues,
            consumer: QueueConsumer {
                name: kube_service.name.clone(),
                container: None,
                workload_type,
            },
        },
    );
    qr_api
        .create(&PostParams::default(), &queue_registry)
        .await
        .expect("Could not create queue splitter in E2E test.");
    println!("MirrordWorkloadQueueRegistry resource created at namespace {} with name {QUEUE_REGISTRY_RESOURCE_NAME}", kube_service.namespace);
}

/// Name of the environment variable that holds the configuration for this app.
///
/// The configuration is expected to be a JSON array of [`SqsQueueEnv`] objects.
/// If the variable is not set, the app will use [`legacy_config`].
/// This is to maintain backwards compatibility with mirrord Operator E2E.
pub const CONFIGURATION_ENV_NAME: &str = "SQS_FORWARDER_CONFIG";

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ForwardingConfig {
    /// Queue from which we should read messages.
    pub from: SqsQueueEnv,
    /// Queue to which we should echo messages.
    pub to: SqsQueueEnv,
}

/// Describes a source of SQS queue name/URL for this app.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SqsQueueEnv {
    /// Name of the environment variable that holds the SQS queue name or URL.
    pub var_name: String,
    /// Whether the environment variable holds a URL or a queue name.
    #[serde(default)]
    pub is_url: bool,
    /// If set, the value of the environment variable will be parsed into a JSON object.
    /// The queue name/URL will be extracted from the field with this name.
    pub json_key: Option<String>,
}

async fn config_map_resource(name: &str, map: BTreeMap<String, String>) -> ConfigMap {
    ConfigMap {
        data: Some(map),
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(get_test_resource_label_map()),
            ..Default::default()
        },
        ..Default::default()
    }
}

struct SqsConsumerServiceConfig {
    with_fallback_json: bool,
    with_env_from: bool,
    with_value_from: bool,
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
    SqsConsumerServiceConfig {
        with_fallback_json,
        with_env_from,
        with_value_from,
    }: SqsConsumerServiceConfig,
    workload_type: TestWorkload,
) -> KubeService {
    let namespace = format!("e2e-tests-sqs-splitting-{}", crate::utils::random_string());

    let mut config_maps = Vec::new();
    if with_value_from {
        config_maps.push(
            config_map_resource(
                CONFIG_MAP_FOR_VALUE_FROM,
                BTreeMap::from_iter([(QUEUE_NAME_ENV_VAR1.into(), queue1.name.clone())]),
            )
            .await,
        );
    }
    if with_env_from {
        config_maps.push(
            config_map_resource(
                CONFIG_MAP_FOR_ENV_FROM,
                BTreeMap::from_iter([(QUEUE2_URL_ENV_VAR.into(), queue2.url.clone())]),
            )
            .await,
        );
    }
    let config_maps = config_maps.is_empty().not().then_some(config_maps);

    let env_var_from_tuple = |(name, value)| EnvVar {
        name,
        value: Some(value),
        value_from: None,
    };

    let queue_name_env_vars = if with_fallback_json {
        if with_env_from || with_value_from {
            panic!("envFrom + fallback_json test case not implemented")
        }
        // Configuration for the deployed test app, to make it expect the queues as a json object.
        let test_service_config = vec![
            ForwardingConfig {
                from: SqsQueueEnv {
                    var_name: QUEUE_JSON_ENV_VAR.to_string(),
                    is_url: false,
                    json_key: Some(QUEUE1_NAME_KEY.to_string()),
                },
                to: SqsQueueEnv {
                    var_name: "SQS_TEST_ECHO_Q_NAME1".into(),
                    is_url: false,
                    json_key: None,
                },
            },
            ForwardingConfig {
                from: SqsQueueEnv {
                    var_name: QUEUE_JSON_ENV_VAR.to_string(),
                    is_url: true,
                    json_key: Some(QUEUE2_URL_KEY.to_string()),
                },
                to: SqsQueueEnv {
                    var_name: "SQS_TEST_ECHO_Q_NAME2".into(),
                    is_url: false,
                    json_key: None,
                },
            },
        ];
        let test_service_config_string = serde_json::to_string(&test_service_config).unwrap();

        let json_for_deployed_app = serde_json::json!({
            QUEUE1_NAME_KEY: queue1.name,
            QUEUE2_URL_KEY: queue2.url
        });

        [
            (
                QUEUE_JSON_ENV_VAR.to_string(),
                serde_json::to_string(&json_for_deployed_app).unwrap(),
            ),
            (
                CONFIGURATION_ENV_NAME.to_string(),
                test_service_config_string,
            ),
        ]
        .into_iter()
        .map(env_var_from_tuple)
        .collect()
    } else {
        let mut env_vars = Vec::new();
        let var1 = if with_value_from {
            EnvVar {
                name: QUEUE_NAME_ENV_VAR1.into(),
                value: None,
                value_from: Some(EnvVarSource {
                    config_map_key_ref: Some(ConfigMapKeySelector {
                        key: QUEUE_NAME_ENV_VAR1.to_string(),
                        name: CONFIG_MAP_FOR_VALUE_FROM.to_string(),
                        optional: Some(false),
                    }),
                    field_ref: None,
                    resource_field_ref: None,
                    secret_key_ref: None,
                }),
            }
        } else {
            EnvVar {
                name: QUEUE_NAME_ENV_VAR1.into(),
                value: Some(queue1.name.clone()),
                value_from: None,
            }
        };
        env_vars.push(var1);
        if with_env_from.not() {
            env_vars.push(EnvVar {
                name: QUEUE2_URL_ENV_VAR.into(),
                value: Some(queue2.url.clone()),
                value_from: None,
            })
        }
        env_vars
    };
    // env vars that are already needed, in all test cases.
    let env = [
        (ECHO_QUEUE_NAME_ENV_VAR1.into(), echo_queue1.name.clone()),
        (ECHO_QUEUE_NAME_ENV_VAR2.into(), echo_queue2.name.clone()),
    ]
    .into_iter()
    .chain(aws_creds)
    .map(env_var_from_tuple)
    .chain(queue_name_env_vars)
    .collect::<Vec<_>>();

    let env_from = with_env_from.then(|| {
        vec![EnvFromSource {
            config_map_ref: Some(ConfigMapEnvSource {
                name: CONFIG_MAP_FOR_ENV_FROM.to_string(),
                optional: Some(false),
            }),
            prefix: None,
            secret_ref: None,
        }]
    });

    let delete_after_fail = std::env::var_os("PRESERVE_FAILED_ENV_NAME").is_none();
    let kube_client = kube_client.clone();
    let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
    let deployment_api: Api<k8s_openapi::api::apps::v1::Deployment> =
        Api::namespaced(kube_client.clone(), &namespace);
    let service_api: Api<k8s_openapi::api::core::v1::Service> =
        Api::namespaced(kube_client.clone(), &namespace);
    let rollout_api: Api<Rollout> = Api::namespaced(kube_client.clone(), &namespace);

    let name = format!("queue-forwarder-{}", crate::utils::random_string());

    // Create namespace
    let namespace_resource: Namespace = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {
            "name": &namespace,
            "labels": {
                crate::utils::TEST_RESOURCE_LABEL.0: crate::utils::TEST_RESOURCE_LABEL.1,
            }
        },
    }))
    .unwrap();
    let namespace_guard = ResourceGuard::create(
        namespace_api.clone(),
        &namespace_resource,
        delete_after_fail,
    )
    .await
    .ok();

    // Create deployment
    let deployment = deployment_from_json(
        &name,
        "ghcr.io/metalbear-co/mirrord-sqs-forwarder:latest",
        serde_json::to_value(env).unwrap(),
        env_from.clone(),
    );
    let (deployment_guard, deployment) =
        ResourceGuard::create(deployment_api.clone(), &deployment, delete_after_fail)
            .await
            .unwrap();

    // Create service
    let service = service_from_json(&name, "ClusterIP");
    let (service_guard, service) =
        ResourceGuard::create(service_api.clone(), &service, delete_after_fail)
            .await
            .unwrap();

    // Wait for pod
    let pod_name = watch::wait_until_pods_ready(&service, 1, kube_client.clone())
        .await
        .into_iter()
        .next()
        .unwrap()
        .metadata
        .name
        .unwrap();

    match workload_type {
        TestWorkload::Deployment => KubeService {
            name: name.clone(),
            namespace: namespace.to_string(),
            service,
            deployment,
            rollout: None,
            pod_name,
            guards: vec![deployment_guard, service_guard],
            namespace_guard: namespace_guard.map(|(guard, _)| guard),
        },
        TestWorkload::ArgoRolloutWithWorkloadRef => {
            let rollout = argo_rollout_from_json(&name, &deployment);
            let (rollout_guard, rollout) =
                ResourceGuard::create(rollout_api.clone(), &rollout, delete_after_fail)
                    .await
                    .unwrap();
            // Scale deployment to zero replicas so only the rollout manages pods
            let patch = serde_json::json!({ "spec": { "replicas": 0 } });
            deployment_api
                .patch(
                    &name,
                    &PatchParams::apply("mirrord-test"),
                    &Patch::Merge(&patch),
                )
                .await
                .expect("Failed to scale deployment to zero replicas");
            watch::wait_until_rollout_available(&name, &namespace, 1, kube_client.clone()).await;
            KubeService {
                name: name.clone(),
                namespace: namespace.to_string(),
                service,
                deployment,
                rollout: Some(rollout),
                pod_name,
                guards: vec![deployment_guard, service_guard, rollout_guard],
                namespace_guard: namespace_guard.map(|(guard, _)| guard),
            }
        }
        TestWorkload::ArgoRolloutWithTemplate => {
            let rollout = argo_rollout_with_template_from_json(&name, &deployment);
            let (rollout_guard, rollout) =
                ResourceGuard::create(rollout_api.clone(), &rollout, delete_after_fail)
                    .await
                    .unwrap();
            watch::wait_until_rollout_available(&name, &namespace, 1, kube_client.clone()).await;
            KubeService {
                name: name.clone(),
                namespace: namespace.to_string(),
                service,
                deployment,
                rollout: Some(rollout),
                pod_name,
                guards: vec![deployment_guard, service_guard, rollout_guard],
                namespace_guard: namespace_guard.map(|(guard, _)| guard),
            }
        }
    }
}

/// Attempts to find the `localstack` service in the `localstack` namespace.
async fn get_localstack_service(kube_client: &Client) -> Option<Service> {
    let service_api = Api::<Service>::namespaced(kube_client.clone(), "localstack");
    service_api.get("localstack").await.ok()
}

/// Waits until the SQS test namespace reaches stable state.
///
/// This check includes:
/// 1. Waiting for the [`MirrordWorkloadQueueRegistry`] [`QUEUE_REGISTRY_RESOURCE_NAME`] to have a
///    non-empty status.
/// 2. Waiting for `expected_sessions` [`MirrordSqsSession`] resources to be ready.
/// 3. Waiting for all pods in the namespace to be ready (phase is `Running` and all containers are
///    ready).
pub async fn wait_for_stable_state(
    kube_client: &Client,
    namespace: &str,
    expected_sessions: usize,
    expected_pods: usize,
) {
    println!(
        "Waiting for {} {} to have status.",
        MirrordWorkloadQueueRegistry::kind(&()),
        QUEUE_REGISTRY_RESOURCE_NAME
    );
    let api = Api::<MirrordWorkloadQueueRegistry>::namespaced(kube_client.clone(), namespace);
    Watcher::new(api, Default::default(), |registries| {
        let registry = registries
            .get(QUEUE_REGISTRY_RESOURCE_NAME)
            .expect("queue registry was not found in the test namespace");
        let queue_names = registry
            .status
            .as_ref()
            .and_then(|status| status.sqs_details.as_ref())
            .map(|details| details.queue_names.clone())
            .unwrap_or_default();
        let ready = queue_names.is_empty().not();
        if ready {
            println!(
                "{} {} has status, queue_names={:?}",
                MirrordWorkloadQueueRegistry::kind(&()),
                QUEUE_REGISTRY_RESOURCE_NAME,
                queue_names
            )
        }
        ready
    })
    .run()
    .await;

    println!(
        "Waiting for exactly {expected_sessions} ready {}.",
        MirrordSqsSession::plural(&())
    );
    let api = Api::<MirrordSqsSession>::namespaced(kube_client.clone(), namespace);
    Watcher::new(api, Default::default(), move |sessions| {
        if sessions.len() != expected_sessions {
            return false;
        }
        sessions.values().for_each(|session| {
            if session.metadata.deletion_timestamp.is_some() {
                panic!(
                    "{} {} is being deleted",
                    MirrordSqsSession::kind(&()),
                    session.metadata.name.as_deref().unwrap_or("<no-name>")
                );
            }
        });
        let ready = sessions.values().all(|session| match &session.status {
            Some(SqsSessionStatus::Ready(..)) => true,
            Some(
                SqsSessionStatus::StartError(error) | SqsSessionStatus::CleanupError { error, .. },
            ) => {
                panic!(
                    "{} {} failed: {error}",
                    MirrordSqsSession::kind(&()),
                    session.metadata.name.as_deref().unwrap_or("<no-name>")
                )
            }
            _ => false,
        });
        if ready {
            println!(
                "{expected_sessions} {} are ready: {sessions:?}",
                MirrordSqsSession::plural(&())
            );
        }
        ready
    })
    .run()
    .await;

    println!("Waiting for exactly {expected_pods} pods to be ready.");
    let api = Api::<Pod>::namespaced(kube_client.clone(), namespace);
    Watcher::new(api, Default::default(), move |pods| {
        if pods.len() != expected_pods {
            return false;
        }
        let ready = pods.values().all(|pod| {
            let Some(status) = &pod.status else {
                return false;
            };
            if status.phase.as_deref() != Some("Running") {
                return false;
            }
            let Some(container_statuses) = &status.container_statuses else {
                return false;
            };
            container_statuses.iter().all(|status| status.ready)
        });
        if ready {
            println!("{expected_pods} pods are ready.");
        }
        ready
    })
    .run()
    .await;
}

/// - Fetch AWS credentials from the operator container.
/// - Create an SQS client (that uses localstack if there).
/// - Create 4 guarded SQS queues with partially random names: 2 queues for the test applications to
///   consume from, and two "echo" queues from the deployed test application to forward messages to,
///   so that the test can verify it received them.
/// - Start a task that waits for 2 SQS Sessions to be ready.
/// - Deploy a consumer service in a new guarded namespace with a partially random name.
/// - Create a `MirrordWorkloadQueueRegistry` resource in the test's namespace.
pub async fn sqs_test_resources(
    kube_client: Client,
    use_regex: bool,
    with_fallback_json: bool,
    with_env_from: bool,
    with_value_from: bool,
    workload_type: TestWorkload,
) -> SqsTestResources {
    let mut guards = Vec::new();

    let aws_creds = fetch_aws_creds(kube_client.clone()).await;
    let mut local_aws_creds = aws_creds.clone();

    let aws_endpoint_url = local_aws_creds.get_mut(AWS_ENDPOINT_ENV).unwrap();
    let localstack_portforwarder = if aws_endpoint_url.contains("localstack.svc.cluster") {
        let localstack = get_localstack_service(&kube_client).await.unwrap();
        let localstack_portforwarder =
            PortForwarder::new_for_service(kube_client.clone(), &localstack, 4566).await;
        *aws_endpoint_url = format!("http://{}", localstack_portforwarder.address());
        Some(localstack_portforwarder)
    } else {
        None
    };

    let sqs_client = get_sqs_client(local_aws_creds).await;

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
        SqsConsumerServiceConfig {
            with_fallback_json,
            with_env_from,
            with_value_from,
        },
        workload_type,
    )
    .await;

    create_queue_registry_resource(
        &kube_client,
        &k8s_service,
        use_regex,
        with_fallback_json.then_some((&queue1, &queue2)),
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
    messages: Vec<TestMessage>,
) {
    println!("Sending messages {messages:?} to queue {}.", queue.name);

    let fifo = queue.name.ends_with(".fifo");
    let group_id = fifo.then_some("e2e-tests".to_string());

    let mut batch_send = sqs_client.send_message_batch().queue_url(&queue.url);
    for (index, message) in messages.into_iter().enumerate() {
        let entry = SendMessageBatchRequestEntryBuilder::default()
            .id(index.to_string())
            .message_body(message.body)
            .message_attributes(
                message.attribute_name,
                MessageAttributeValue::builder()
                    .string_value(message.attribute_value)
                    .data_type("String")
                    .build()
                    .unwrap(),
            )
            .set_message_group_id(group_id.clone())
            .set_message_deduplication_id(fifo.then_some(index.to_string()))
            .build()
            .unwrap();
        batch_send = batch_send.entries(entry);
    }
    batch_send
        .send()
        .await
        .expect("Sending SQS messages failed.");
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

#[derive(Debug)]
pub struct TestMessage {
    pub body: String,
    pub attribute_name: String,
    pub attribute_value: String,
}

use k8s_openapi::api::core::v1::Secret;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration to use when creating operator's Kafka client.
/// Resources of this kind should live in the operator's namespace.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "queues.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordKafkaClientConfig",
    namespaced,
    printcolumn = r#"{"name":"PARENT", "type":"string", "description":"Name of parent configuration.", "jsonPath":".spec.parent"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordKafkaClientConfigSpec {
    /// Name of parent resource to use as base when resolving final configuration.
    pub parent: Option<String>,

    /// Properties to set.
    ///
    /// When performing Kafka splitting, the operator will override `group.id` property.
    ///
    /// The list of all available properties can be found [here](https://github.com/confluentinc/librdkafka/blob/master/CONFIGURATION.md).
    pub properties: Vec<MirrordKafkaClientProperty>,

    /// Properties to set from a secret.
    ///
    /// The secret is resolved when resolving the rest of the final configuration.
    pub load_from_secret: Option<Secret>,
}

/// Property to use when creating operator's Kafka client.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct MirrordKafkaClientProperty {
    /// Name of the property, e.g `bootstrap.servers`.
    pub name: String,

    /// Value for the property, e.g `kafka.default.svc.cluster.local:9092`.
    /// `null` clears the property from parent resource when resolving the final configuration.
    pub value: Option<String>,
}

/// Defines splittable Kafka topics consumed by some workload living in the same namespace.
///
/// # Concurrent splitting
///
/// Concurrent Kafka splitting sessions are allowed, as long as they use the same topic id or their
/// topics' `nameSources` do not overlap.
///
/// # Example
///
/// ```yaml
/// apiVersion: queues.mirrord.metalbear.co/v1alpha
/// kind: MirrordKafkaTopicsConsumer
/// metadata:
///   name: example
///   namespace: default
/// spec:
///   consumerName: example-deployment
///   consumerApiVersion: apps/v1
///   consumerKind: Deployment
///   topics:
///     - id: example-topic
///       nameSources:
///         - directEnvVar:
///             container: example-container
///             name: KAFKA_TOPIC_NAME
///       groupIdSources:
///         - directEnvVar:
///             container: example-container
///             name: KAFKA_GROUP_ID
///       clientConfig: example-config
/// ```
///
/// 1. Creating the resource below will enable Kafka splitting on a deployment `example-deployment`
///    living in namespace `default`. Id `example-topic` can be then used in the mirrord config to
///    split the topic for the duration of the mirrord session.
///
/// 2. Topic name will be resolved based on `example-deployment`'s pod template by extracting value
///    of variable `KAFKA_TOPIC_NAME` defined directly in `example-container`.
///
/// 3. Consumer group id used by the mirrord operator will be resolved based on
///    `example-deployment`'s pod template by extracting value of variable `KAFKA_GROUP_ID` defined
///    directly in `example-container`.
///
/// 4. For the duration of the session, `example-deployment` will be patched - the mirrord operator
///    will substitute topic name in `KAFKA_TOPIC_NAME` variable with a name of an ephemeral Kafka
///    topic.
///
/// 5. Local application will see a different value of the `KAFKA_TOPIC_NAME` - it will be a name of
///    another ephemeral Kafka topic.
///
/// 6. `MirrordKafkaClientConfig` named `example-config` living in mirrord operator's namespace will
///    be used to manage ephemeral Kafka topics and consume/produce messages.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "queues.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordKafkaTopicsConsumer",
    namespaced,
    printcolumn = r#"{"name":"CONSUMER-NAME", "type":"string", "description":"Name of the topic consumer workload.", "jsonPath":".spec.consumerName"}"#,
    printcolumn = r#"{"name":"CONSUMER-KIND", "type":"string", "description":"Kind of the topic consumer workload.", "jsonPath":".spec.consumerKind"}"#,
    printcolumn = r#"{"name":"CONSUMER-API-VERSION", "type":"string", "description":"Api version of the topic consumer workload.", "jsonPath":".spec.consumerApiVersion"}"#,
    printcolumn = r#"{"name":"CONSUMER-RESTART-TIMEOUT", "type":"string", "description":"Timeout for consumer workload restart.", "jsonPath":".spec.consumerRestartTimeout"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordKafkaTopicsConsumerSpec {
    /// Workload name, for example `my-deployment`.
    pub consumer_name: String,

    /// Workload kind, for example `Deployment`.
    pub consumer_kind: String,

    /// Workload api version, for example `apps/v1`.
    pub consumer_api_version: String,

    /// Timeout for waiting until workload patch takes effect, that is at least one pod reads from
    /// the ephemeral topic.
    ///
    /// Specified in seconds. Defaults to 60s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer_restart_timeout: Option<u32>,

    /// List of consumed splittable topics.
    pub topics: Vec<KafkaTopicDetails>,
}

/// Splittable Kafka topic consumed by some remote target.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KafkaTopicDetails {
    /// Id of this topic. Can be used in mirrord config to identify this topic.
    pub id: String,

    /// All occurrences of this topic's name in the workload's pod template.
    pub name_sources: Vec<TopicPropertySource>,

    /// All occurrences of this topic's group id in the workload's pod template.
    pub group_id_sources: Vec<TopicPropertySource>,

    /// Links to [`MirrordKafkaClientConfig`] in the operator's namespace.
    /// This config will be used to manage ephemeral Kafka topics and consume/produce messages.
    pub client_config: String,
}

/// Source of some topic property required for Kafka splitting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema, Hash)]
#[serde(rename_all = "camelCase")]
pub enum TopicPropertySource {
    /// Environment variable with value defined directly in the pod template.
    DirectEnvVar(EnvVarLocation),
}

/// Location of an environment variable defined in the pod template.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema, Hash)]
#[serde(rename_all = "camelCase")]
pub struct EnvVarLocation {
    /// Name of the container.
    pub container: String,

    /// Name of the variable.
    pub variable: String,
}

/// Ephemeral topic created in your Kafka cluster for the purpose of running a Kafka splitting
/// session.
///
/// Resources of this kind should live in the operator's namespace. They will be used to clean up
/// topics that are no longer used.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq, Hash)]
#[kube(
    group = "queues.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordKafkaEphemeralTopic",
    namespaced,
    printcolumn = r#"{"name":"NAME", "type":"string", "description":"Name of the topic.", "jsonPath":".spec.name"}"#,
    printcolumn = r#"{"name":"CLIENT-CONFIG", "type":"string", "description":"Name of MirrordKafkaClientProperties to use when creating Kafka client.", "jsonPath":".spec.clientConfig"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordKafkaEphemeralTopicSpec {
    /// Name of the topic.
    pub name: String,
    /// Links to [`MirrordKafkaClientConfigSpec`] resource living in the same namespace.
    pub client_config: String,
}

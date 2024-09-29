use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Properties to use when creating operator's Kafka client.
/// Resources of this kind should live in the operator's namespace.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "queues.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordKafkaClientProperties",
    namespaced,
    printcolumn = r#"{"name":"PARENT", "type":"string", "description":"Name of parent configuration.", "jsonPath":".spec.parent"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordKafkaClientPropertiesSpec {
    /// Name of parent resource to use as base when resolving final configuration.
    pub parent: Option<String>,
    /// Properties to set.
    pub properties: Vec<MirrordKafkaClientProperty>,
}

/// Property to use when creating operator's Kafka client.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct MirrordKafkaClientProperty {
    /// Name of the property, e.g `bootstrap.servers`.
    pub name: String,
    /// Value for the property, e.g `kafka.default.svc.cluster.local:9092`.
    /// `null` clears the property from parent resource when resolving the final configuration.
    pub value: Option<String>,
}

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "queues.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordKafkaSplittableTopic",
    namespaced
)]
pub struct MirrordKafkaSplittableTopicSpec {
    /// All occurrences of this topic's name in the pod spec.
    pub name_sources: Vec<TopicPropertySource>,
    /// All occurrences of this topic's group id in the pod spec.
    pub group_id_sources: Vec<TopicPropertySource>,
    /// Links to [`MirrordKafkaClientProperties`] in the operator's namespace.
    pub client_properties: String,
    pub workload_name: String,
    pub workload_kind: String,
    pub workload_api_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TopicPropertySource {
    DirectEnvVar(DirectEnvVar),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirectEnvVar {
    pub container: String,
    pub name: String,
}

/// Created temporary topic in a Kafka cluster.
/// Resources of this kind should live in the operator's namespace.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "queues.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordKafkaTemporaryTopic",
    namespaced,
    printcolumn = r#"{"name":"NAME", "type":"string", "description":"Name of the topic.", "jsonPath":".spec.name"}"#,
    printcolumn = r#"{"name":"CLIENT-PROPERTIES", "type":"string", "description":"Name of MirrordKafkaClientProperties to use when creating Kafka client.", "jsonPath":".spec.clientProperties"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordKafkaTemporaryTopicSpec {
    /// Name of the topic.
    pub name: String,
    /// Links to [`MirrordKafkaClientProperties`] resource living in the same namespace.
    pub client_properties: String,
}

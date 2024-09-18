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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KafkaTopicDetails {
    /// Where to find Kafka topic name in the target spec.
    pub name_source: TopicPropertySource,
    /// Where to find consumer group id in the target spec.
    pub group_id_source: TopicPropertySource,
    /// Links to [`MirrordKafkaClientProperties`] in the operator's namespace.
    pub admin_client_properties: String,
    /// Links to [`MirrordKafkaClientProperties`] in the operator's namespace.
    pub producer_client_properties: String,
    /// Links to [`MirrordKafkaClientProperties`] in the operator's namespace.
    pub consumer_client_properties: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TopicPropertySource {
    EnvVar(String),
}

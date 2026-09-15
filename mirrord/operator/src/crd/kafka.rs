use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    category = "mirrord",
    namespaced,
    printcolumn = r#"{"name":"Name", "type":"string", "description":"Name of the topic.", "jsonPath":".spec.name"}"#,
    printcolumn = r#"{"name":"Client Config", "type":"string", "description":"Name of MirrordKafkaClientProperties to use when creating Kafka client.", "jsonPath":".spec.clientConfig"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "description":"Time since the resource was created.", "jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordKafkaEphemeralTopicSpec {
    /// Name of the topic.
    pub name: String,
    /// Links to `operator_crd::crd::kafka::MirrordKafkaClientConfig` resource living in the same
    /// namespace.
    pub client_config: String,
}

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Generic external resource created by the operator.
///
/// For future compatibility and to avoid any schema issues, every resource type is represented in a
/// separate optional field.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq, Hash)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterExternalResource"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterExternalResourceSpec {
    /// Temporary SQS queue created for queue splitting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqs_queue: Option<SqsQueue>,
    /// Resource allocated in a RabbitMQ cluster for queue splitting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rmq_resource: Option<RmqResource>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct SqsQueue {
    pub name_or_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct RmqResource {
    pub name: String,
    pub kind: RmqResourceKind,
    /// Name of the `MirrordPropertiesList` that should be used to resolve client configuration.
    pub properties_list_name: String,
    /// Namespace of the `MirrordPropertiesList` that should be used to resolve client
    /// configuration.
    pub properties_list_namespace: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum RmqResourceKind {
    Queue,
    Exchange,
    #[serde(other)]
    Unknown,
}

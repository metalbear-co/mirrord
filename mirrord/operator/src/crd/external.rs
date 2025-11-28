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
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct SqsQueue {
    pub name_or_url: String,
}

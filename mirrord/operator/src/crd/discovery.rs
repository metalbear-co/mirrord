use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "SqsQueueDiscovery",
    status = "SqsQueueDiscoveryStatus",
    plural = "sqsqueuediscoveries",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct SqsQueueDiscoverySpec {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub container: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SqsQueueDiscoveryStatus {
    #[serde(default)]
    pub input_queues: Vec<String>,
    #[serde(default)]
    pub output_queues: Vec<String>,
}

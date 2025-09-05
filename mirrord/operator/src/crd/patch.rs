use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, Hash)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterWorkloadPatch",
    printcolumn = r#"{"name":"WORKLOAD-API-VERSION", "type":"string", "description":"API version of the workload.", "jsonPath":".spec.workloadRef.apiVersion"}"#,
    printcolumn = r#"{"name":"WORKLOAD-KIND", "type":"string", "description":"Kind of the workload.", "jsonPath":".spec.workloadRef.kind"}"#,
    printcolumn = r#"{"name":"WORKLOAD-NAME", "type":"string", "description":"Name of the workload.", "jsonPath":".spec.workloadRef.name"}"#,
    printcolumn = r#"{"name":"WORKLOAD-NAMESPACE", "type":"string", "description":"API version of the workload.", "jsonPath":".spec.workloadRef.namespace"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterWorkloadPatchSpec {
    pub workload_ref: PatchedWorkloadRef,
    pub requests: Vec<PatchRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchedWorkloadRef {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchRequest {
    pub uid: String,
    pub extra_inline_env: Vec<ExtraInlineEnv>,
    pub replicas: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtraInlineEnv {
    pub container: String,
    pub variable: String,
    pub value: String,
}

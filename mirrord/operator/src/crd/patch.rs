use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Represents operator patch state of some workload.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, Hash)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterWorkloadPatch",
    printcolumn = r#"{"name":"WORKLOAD-API-VERSION", "type":"string", "description":"API version of the workload.", "jsonPath":".spec.workloadRef.apiVersion"}"#,
    printcolumn = r#"{"name":"WORKLOAD-KIND", "type":"string", "description":"Kind of the workload.", "jsonPath":".spec.workloadRef.kind"}"#,
    printcolumn = r#"{"name":"WORKLOAD-NAME", "type":"string", "description":"Name of the workload.", "jsonPath":".spec.workloadRef.name"}"#,
    printcolumn = r#"{"name":"WORKLOAD-NAMESPACE", "type":"string", "description":"Namespace of the workload.", "jsonPath":".spec.workloadRef.namespace"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterWorkloadPatchSpec {
    /// Reference to the workload being patched.
    pub workload_ref: PatchedWorkloadRef,
    /// Collection of individual [`PatchRequest`]s to be applied.
    pub requests: Vec<PatchRequest>,
}

/// Reference to a Kubernetes workload patched with [`MirrordClusterWorkloadPatch`].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchedWorkloadRef {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
}

/// Request for a patch to be applied to a Kubernetes workload.
///
/// Part of [`MirrordClusterWorkloadPatch`].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchRequest {
    /// Opaque identifier of this request's owner.
    pub owner: String,
    /// Environment variables to be set in the pod template.
    pub env_vars: Vec<EnvVar>,
    /// Number of replicas to be set in the workload spec.
    pub replicas: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub container: String,
    pub variable: String,
    pub value: String,
}

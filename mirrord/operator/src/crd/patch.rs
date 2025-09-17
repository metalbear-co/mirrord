use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Internal resource, representing operator patch state of some workload.
///
/// This is an aggregated state of accepted [`MirrordClusterWorkloadPatchRequest`]s.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
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
    /// Collection of accepted [`MirrordClusterWorkloadPatchRequest`]s.
    ///
    /// These requests will be enforced on the workload.
    pub requests: Vec<AcceptedPatchRequest>,
}

/// Reference to a Kubernetes workload patched with [`MirrordClusterWorkloadPatch`] and
/// [`MirrordClusterWorkloadPatchRequest`].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchedWorkloadRef {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
}

/// An accepted [`MirrordClusterWorkloadPatchRequest`], part of [`MirrordClusterWorkloadPatch`].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedPatchRequest {
    /// UID of the origin [`MirrordClusterWorkloadPatchRequest`] resource.
    pub uid: String,
    /// Environment variables to be set in the pod template.
    pub env_vars: Vec<EnvVar>,
    /// Number of replicas to be set in the workload spec.
    ///
    /// [`None`] means that replicas should not be patched.
    pub replicas: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub container: String,
    pub variable: String,
    pub value: String,
}

/// Internal resource, representing an operator request to patch some workload.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterWorkloadPatchRequest",
    printcolumn = r#"{"name":"WORKLOAD-API-VERSION", "type":"string", "description":"API version of the workload.", "jsonPath":".spec.workloadRef.apiVersion"}"#,
    printcolumn = r#"{"name":"WORKLOAD-KIND", "type":"string", "description":"Kind of the workload.", "jsonPath":".spec.workloadRef.kind"}"#,
    printcolumn = r#"{"name":"WORKLOAD-NAME", "type":"string", "description":"Name of the workload.", "jsonPath":".spec.workloadRef.name"}"#,
    printcolumn = r#"{"name":"WORKLOAD-NAMESPACE", "type":"string", "description":"Namespace of the workload.", "jsonPath":".spec.workloadRef.namespace"}"#,
    status = "MirrordClusterWorkloadPatchRequestStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterWorkloadPatchRequestSpec {
    /// Reference to the workload being patched.
    pub workload_ref: PatchedWorkloadRef,
    /// Environment variables to be set in the pod template.
    pub env_vars: Vec<EnvVar>,
    /// Number of replicas to be set in the workload spec.
    ///
    /// [`None`] means that replicas should not be patched.
    pub replicas: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Hash, PartialEq, Eq)]
pub struct MirrordClusterWorkloadPatchRequestStatus {
    pub accepted: bool,
}

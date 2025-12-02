use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterSession",
    status = "MirrordClusterSessionStatus",
    printcolumn = r#"{"name":"USER ID", "type":"string", "description":"User unique ID..", "jsonPath":".spec.owner.userId"}"#,
    printcolumn = r#"{"name":"USERNAME", "type":"string", "description":"User local POSIX name.", "jsonPath":".spec.owner.username"}"#,
    printcolumn = r#"{"name":"HOSTNAME", "type":"string", "description":"User hostname.", "jsonPath":".spec.owner.hostname"}"#,
    printcolumn = r#"{"name":"K8S USER", "type":"string", "description":"User Kubernetes name.", "jsonPath":".spec.owner.k8sUsername"}"#,
    printcolumn = r#"{"name":"NAMESPACE", "type":"string", "description":"Namespace of the session.", "jsonPath":".spec.namespace"}"#,
    printcolumn = r#"{"name":"TARGET", "type":"string", "description":"Target of the session.", "jsonPath":".spec.target"}"#,
    printcolumn = r#"{"name":"STARTED AT", "type":"date", "description":"Time when the session was started.", "jsonPath":".metadata.creationTimestamp"}"#,
    printcolumn = r#"{"name":"CLOSED AT", "type":"date", "description":"Time when the session was closed.", "jsonPath":".metadata.deletionTimestamp"}"#,
    printcolumn = r#"{"name":"CLOSE REASON", "type":"string", "description":"Reason for which the session was closed.", "jsonPath":".status.closed.reason"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionSpec {
    /// Resources needed to report session metrics to the mirrord Jira app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_metrics: Option<SessionJiraMetrics>,
    /// Owner of this session
    pub owner: SessionOwner,
    /// Kubernetes namespace of the session.
    pub namespace: String,
    /// Target of the session.
    ///
    /// None for targetless sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<SessionTarget>,

    /// CI info when a session is started with `mirrord ci start`.
    ///
    /// Most of the values here will be hashed before being sent to the license-server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_info: Option<MirrordCiInfo>,

    /// Copy target configuration for this session.
    ///
    /// Set when the session uses a copied pod.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_target: Option<SessionCopyTarget>,
}

/// Describes an owner of a mirrord session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOwner {
    /// Unique ID.
    pub user_id: String,
    /// Name of the POSIX user that executed the CLI command.
    pub username: String,
    /// Hostname of the machine where the CLI command was executed.
    pub hostname: String,
    /// Name of the Kubernetes user who's identity was assumed by the CLI.
    pub k8s_username: String,
}

/// Describes a target of a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTarget {
    /// Kubernetes resource apiVersion.
    pub api_version: String,
    /// Kubernetes resource kind.
    pub kind: String,
    /// Kubernetes resource name.
    pub name: String,
    /// Name of the container defined in the Pod spec.
    pub container: String,
}

/// Resources needed to report session metrics to the mirrord Jira app.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJiraMetrics {
    /// The user's current git branch.
    pub branch_name: String,
}

/// Describes copy target configuration for a session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCopyTarget {
    /// Whether the original target should be scaled down.
    pub scaledown: bool,
}

/// Status of a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionStatus {
    /// Last time when the session was observed to have an open user connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_timestamp: Option<MicroTime>,
    /// If the session has been closed, describes the reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<SessionClosed>,
    /// Copy target status for sessions using a copied pod.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_target: Option<SessionCopyTargetStatus>,
}

/// Status of the copy target for a session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCopyTargetStatus {
    /// Name of the CopyTargetCrd, used by CLI when making connections.
    pub name: String,
    /// Status of the produced copied pod.
    ///
    /// `None` if there is no valid pod at the moment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copied_pod_status: Option<CopiedPodStatus>,
}

/// Status of a copied pod.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CopiedPodStatus {
    /// Name of the copied pod.
    pub name: String,
    /// Namespace of the copied pod.
    pub namespace: String,
    /// Name of the target container in the copied pod.
    pub target_container: String,
}

/// Describes the reason for with a mirrord session was closed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionClosed {
    /// Short reason in PascalCase.
    pub reason: String,
    /// Optional human friendly message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Information about the CI session started from `mirrord ci start`.
///
/// These values are passed to the operator, and handled by the `ci_controller`, at some point
/// they're sent to the license-server, but as hashed `u64`s (most are hashed).
///
/// ## `TryFrom<http::HeaderValue>`
///
/// Implements this conversion so we can easily send this as a request header in
/// `OperatorApi::connect_target`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MirrordCiInfo {
    /// CI vendor, e.g. "github", "gitlab", ...
    pub vendor: Option<String>,

    /// Name of the git branch.
    pub branch_name: Option<String>,
}

#[cfg(feature = "client")]
impl TryFrom<http::HeaderValue> for MirrordCiInfo {
    type Error = serde_json::Error;

    fn try_from(value: http::HeaderValue) -> Result<Self, Self::Error> {
        serde_json::from_slice(value.as_bytes())
    }
}

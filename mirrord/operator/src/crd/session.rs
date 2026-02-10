use std::fmt;

use k8s_openapi::{
    Resource,
    api::{
        apps::v1::{Deployment, ReplicaSet, StatefulSet},
        batch::v1::{CronJob, Job},
        core::v1::{Pod, Service},
    },
    apimachinery::pkg::apis::meta::v1::MicroTime,
};
use kube::CustomResource;
use mirrord_config::{
    config::ConfigError,
    target::{Target, TargetDisplay},
};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_info: Option<SessionCiInfo>,

    /// Copy target configuration for this session.
    ///
    /// Set when the session uses a copied pod.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_target: Option<SessionCopyTarget>,

    /// Multi-cluster: name of the parent MirrordMultiClusterSession.
    ///
    /// When set, this is a child session created by the Primary operator
    /// as part of a multi-cluster session. The parent session coordinates
    /// all child sessions across clusters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_cluster_parent_name: Option<String>,
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

impl fmt::Display for SessionTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.name)?;
        if !self.container.is_empty() {
            write!(f, "/container/{}", self.container)?;
        }
        Ok(())
    }
}

impl From<&Target> for SessionTarget {
    fn from(target: &Target) -> Self {
        #[cfg(feature = "client")]
        use mirrord_kube::api::kubernetes::rollout::Rollout;

        let api_version = match target {
            Target::CronJob(_) => <CronJob as Resource>::API_VERSION,
            Target::Deployment(_) => <Deployment as Resource>::API_VERSION,
            Target::Job(_) => <Job as Resource>::API_VERSION,
            Target::Pod(_) => <Pod as Resource>::API_VERSION,
            Target::Service(_) => <Service as Resource>::API_VERSION,
            Target::StatefulSet(_) => <StatefulSet as Resource>::API_VERSION,
            Target::ReplicaSet(_) => <ReplicaSet as Resource>::API_VERSION,
            Target::Targetless => "",
            #[cfg(feature = "client")]
            Target::Rollout(_) => <Rollout as Resource>::API_VERSION,
            #[cfg(not(feature = "client"))]
            Target::Rollout(_) => "argoproj.io/v1alpha1",
        };

        Self {
            api_version: api_version.to_owned(),
            kind: target.type_().to_owned(),
            name: target.name().to_owned(),
            container: target.container().cloned().unwrap_or_default(),
        }
    }
}

impl SessionTarget {
    /// Parse into a [`Target`] by reconstructing the canonical target path string and parsing it.
    pub fn as_target(&self) -> Result<Target, ConfigError> {
        self.to_string().parse()
    }
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
/// We try to get some of these fields automatically, but for some that we cannot, the user may
/// pass them as cli args to `mirrord ci start`, see `cli::ci::StartArgs`.
///
/// These values are passed to the operator, and handled by the `ci_controller`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCiInfo {
    /// CI provider, e.g. "github", "gitlab", ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Staging, production, test, nightly, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Pipeline/job name, e.g. "e2e-tests".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,

    /// PR, manual, push, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

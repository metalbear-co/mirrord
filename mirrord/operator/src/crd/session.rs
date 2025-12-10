use std::fmt;

use k8s_openapi::{
    Resource as _,
    api::{
        apps::v1::{Deployment, ReplicaSet, StatefulSet},
        batch::v1::{CronJob, Job},
        core::v1::{Pod, Service},
    },
    apimachinery::pkg::apis::meta::v1::MicroTime,
};
use kube::CustomResource;
use mirrord_config::{
    feature::network::incoming::ConcurrentSteal,
    target::{
        Target, TargetConfig, cron_job::CronJobTarget, deployment::DeploymentTarget,
        job::JobTarget, pod::PodTarget, replica_set::ReplicaSetTarget, rollout::RolloutTarget,
        service::ServiceTarget, stateful_set::StatefulSetTarget,
    },
};
use mirrord_kube::api::kubernetes::{AgentKubernetesConnectInfo, rollout::Rollout};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    printcolumn = r#"{"name":"AGENT_LIMIT", "type":"string", "description":"Relative or absolute limit of agents spawns.", "jsonPath":".spec.agentLimit"}"#,
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
    /// State of concurrent steal
    #[serde(default)]
    pub on_concurrent_steal: ConcurrentSteal,
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
}

impl MirrordClusterSessionSpec {
    pub fn as_target(&self) -> Result<Target, SessionTarget> {
        let Some(target) = &self.target else {
            return Ok(Target::Targetless);
        };

        match (target.api_version.as_str(), target.kind.as_str()) {
            (Deployment::API_VERSION, Deployment::KIND) => {
                Ok(Target::Deployment(DeploymentTarget {
                    deployment: target.name.clone(),
                    container: Some(target.container.clone()),
                }))
            }
            (Pod::API_VERSION, Pod::KIND) => Ok(Target::Pod(PodTarget {
                pod: target.name.clone(),
                container: Some(target.container.clone()),
            })),
            (Rollout::API_VERSION, Rollout::KIND) => Ok(Target::Rollout(RolloutTarget {
                rollout: target.name.clone(),
                container: Some(target.container.clone()),
            })),
            (Job::API_VERSION, Job::KIND) => Ok(Target::Job(JobTarget {
                job: target.name.clone(),
                container: Some(target.container.clone()),
            })),
            (CronJob::API_VERSION, CronJob::KIND) => Ok(Target::CronJob(CronJobTarget {
                cron_job: target.name.clone(),
                container: Some(target.container.clone()),
            })),
            (StatefulSet::API_VERSION, StatefulSet::KIND) => {
                Ok(Target::StatefulSet(StatefulSetTarget {
                    stateful_set: target.name.clone(),
                    container: Some(target.container.clone()),
                }))
            }
            (Service::API_VERSION, Service::KIND) => Ok(Target::Service(ServiceTarget {
                service: target.name.clone(),
                container: Some(target.container.clone()),
            })),
            (ReplicaSet::API_VERSION, ReplicaSet::KIND) => {
                Ok(Target::ReplicaSet(ReplicaSetTarget {
                    replica_set: target.name.clone(),
                    container: Some(target.container.clone()),
                }))
            }
            _ => Err(target.clone()),
        }
    }
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

#[derive(Clone, Debug, Deserialize, Hash, Eq, PartialEq, Serialize, JsonSchema)]
pub struct AgentPodTarget {
    /// Target namespace
    pub namespace: String,

    /// Target name
    pub name: String,

    /// Target's container id
    pub container_id: String,

    /// Target's container name
    pub container_name: String,

    /// Target's pod uid
    #[schemars(with = "String")]
    pub uid: Uuid,
}

impl fmt::Display for AgentPodTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "{}/{}/{}",
            self.namespace, self.name, self.container_name
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Hash, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AgentTarget {
    Targetless {
        /// Target namespace
        namespace: String,

        /// Target namespace uid
        #[schemars(with = "String")]
        uid: Uuid,
    },
    Pod(AgentPodTarget),
}

impl AgentTarget {
    /// Target's namespace
    pub fn namespace(&self) -> &str {
        match self {
            AgentTarget::Targetless { namespace, .. } => namespace,
            AgentTarget::Pod(AgentPodTarget { namespace, .. }) => namespace,
        }
    }

    pub fn uid(&self) -> Uuid {
        match self {
            AgentTarget::Targetless { uid, .. } => *uid,
            AgentTarget::Pod(AgentPodTarget { uid, .. }) => *uid,
        }
    }

    pub fn as_target_config(&self) -> TargetConfig {
        TargetConfig {
            path: match self {
                AgentTarget::Targetless { .. } => None,
                AgentTarget::Pod(target) => Some(Target::Pod(PodTarget {
                    pod: target.name.clone(),
                    container: Some(target.container_name.clone()),
                })),
            },
            namespace: Some(self.namespace().to_string()),
        }
    }
}

impl From<AgentPodTarget> for AgentTarget {
    fn from(pod_target: AgentPodTarget) -> Self {
        AgentTarget::Pod(pod_target)
    }
}

impl fmt::Display for AgentTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentTarget::Targetless { namespace, .. } => write!(f, "targetless/{namespace}"),
            AgentTarget::Pod(pod) => write!(f, "pod/{pod}"),
        }
    }
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionAgent {
    /// Agent connection info to target's agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<AgentKubernetesConnectInfo>,
    /// If the agent is of the ephemeral variaty
    pub ephemeral: bool,
    /// Agent spawn error if there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorMessage>,
    /// The phase of agent's pod.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Resolved agent target.
    pub target: AgentTarget,
}

/// Describes an owner of a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionStatus {
    /// Running agents status
    #[serde(default)]
    pub agents: Vec<MirrordClusterSessionAgent>,
    /// Last time when the session was observed to have an open user connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_timestamp: Option<MicroTime>,
    /// If the session has been closed, describes the reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<ErrorMessage>,
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
pub struct ErrorMessage {
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

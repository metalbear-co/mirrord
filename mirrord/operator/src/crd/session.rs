use std::collections::BTreeMap;

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
        Target, cron_job::CronJobTarget, deployment::DeploymentTarget, job::JobTarget,
        pod::PodTarget, replica_set::ReplicaSetTarget, rollout::RolloutTarget,
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
                    container: target.container.clone(),
                }))
            }
            (Pod::API_VERSION, Pod::KIND) => Ok(Target::Pod(PodTarget {
                pod: target.name.clone(),
                container: target.container.clone(),
            })),
            (Rollout::API_VERSION, Rollout::KIND) => Ok(Target::Rollout(RolloutTarget {
                rollout: target.name.clone(),
                container: target.container.clone(),
            })),
            (Job::API_VERSION, Job::KIND) => Ok(Target::Job(JobTarget {
                job: target.name.clone(),
                container: target.container.clone(),
            })),
            (CronJob::API_VERSION, CronJob::KIND) => Ok(Target::CronJob(CronJobTarget {
                cron_job: target.name.clone(),
                container: target.container.clone(),
            })),
            (StatefulSet::API_VERSION, StatefulSet::KIND) => {
                Ok(Target::StatefulSet(StatefulSetTarget {
                    stateful_set: target.name.clone(),
                    container: target.container.clone(),
                }))
            }
            (Service::API_VERSION, Service::KIND) => Ok(Target::Service(ServiceTarget {
                service: target.name.clone(),
                container: target.container.clone(),
            })),
            (ReplicaSet::API_VERSION, ReplicaSet::KIND) => {
                Ok(Target::ReplicaSet(ReplicaSetTarget {
                    replica_set: target.name.clone(),
                    container: target.container.clone(),
                }))
            }
            _ => Err(target.clone()),
        }
    }

    pub fn with_target(self, target: Target) -> Self {
        MirrordClusterSessionSpec {
            target: match target {
                Target::Deployment(target) => Some(SessionTarget {
                    api_version: Deployment::API_VERSION.to_owned(),
                    kind: Deployment::KIND.to_owned(),
                    name: target.deployment,
                    container: target.container,
                }),
                Target::Pod(target) => Some(SessionTarget {
                    api_version: Pod::API_VERSION.to_owned(),
                    kind: Pod::KIND.to_owned(),
                    name: target.pod,
                    container: target.container,
                }),
                Target::Rollout(target) => Some(SessionTarget {
                    api_version: Rollout::API_VERSION.to_owned(),
                    kind: Rollout::KIND.to_owned(),
                    name: target.rollout,
                    container: target.container,
                }),
                Target::Job(target) => Some(SessionTarget {
                    api_version: Job::API_VERSION.to_owned(),
                    kind: Job::KIND.to_owned(),
                    name: target.job,
                    container: target.container,
                }),
                Target::CronJob(target) => Some(SessionTarget {
                    api_version: CronJob::API_VERSION.to_owned(),
                    kind: CronJob::KIND.to_owned(),
                    name: target.cron_job,
                    container: target.container,
                }),
                Target::StatefulSet(target) => Some(SessionTarget {
                    api_version: StatefulSet::API_VERSION.to_owned(),
                    kind: StatefulSet::KIND.to_owned(),
                    name: target.stateful_set,
                    container: target.container,
                }),
                Target::Service(target) => Some(SessionTarget {
                    api_version: Service::API_VERSION.to_owned(),
                    kind: Service::KIND.to_owned(),
                    name: target.service,
                    container: target.container,
                }),
                Target::ReplicaSet(target) => Some(SessionTarget {
                    api_version: ReplicaSet::API_VERSION.to_owned(),
                    kind: ReplicaSet::KIND.to_owned(),
                    name: target.replica_set,
                    container: target.container,
                }),
                Target::Targetless => None,
            },
            ..self
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
    pub container: Option<String>,
}

/// Resources needed to report session metrics to the mirrord Jira app.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJiraMetrics {
    /// The user's current git branch.
    pub branch_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionAgent {
    /// Agent connection info to target's agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<AgentKubernetesConnectInfo>,
    /// If the agent is of the ephemeral variaty
    pub ephemeral: bool,
    /// Agent's container id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    /// Agent spawn error if there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorMessage>,
    /// The phase of agent's pod.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Resolved agent target.
    pub target: SessionTarget,
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

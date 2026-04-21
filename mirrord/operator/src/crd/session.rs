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
use mirrord_config::target::Target;
use mirrord_kube::api::kubernetes::rollout::Rollout;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOwner {
    pub user_id: String,
    pub username: String,
    pub hostname: String,
    pub k8s_username: String,
}

impl fmt::Display for SessionOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}@{}",
            self.username, self.k8s_username, self.hostname,
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTarget {
    pub api_version: String,
    pub kind: String,
    pub name: String,
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

impl SessionTarget {
    pub fn from_config(target: Target) -> Option<Self> {
        match target {
            Target::Deployment(t) => Some(Self {
                api_version: <Deployment as Resource>::API_VERSION.to_owned(),
                kind: <Deployment as Resource>::KIND.to_owned(),
                name: t.deployment,
                container: t.container?,
            }),
            Target::Pod(t) => Some(Self {
                api_version: <Pod as Resource>::API_VERSION.to_owned(),
                kind: <Pod as Resource>::KIND.to_owned(),
                name: t.pod,
                container: t.container?,
            }),
            Target::Rollout(t) => Some(Self {
                api_version: <Rollout as Resource>::API_VERSION.to_owned(),
                kind: <Rollout as Resource>::KIND.to_owned(),
                name: t.rollout,
                container: t.container?,
            }),
            Target::Job(t) => Some(Self {
                api_version: <Job as Resource>::API_VERSION.to_owned(),
                kind: <Job as Resource>::KIND.to_owned(),
                name: t.job,
                container: t.container?,
            }),
            Target::CronJob(t) => Some(Self {
                api_version: <CronJob as Resource>::API_VERSION.to_owned(),
                kind: <CronJob as Resource>::KIND.to_owned(),
                name: t.cron_job,
                container: t.container?,
            }),
            Target::StatefulSet(t) => Some(Self {
                api_version: <StatefulSet as Resource>::API_VERSION.to_owned(),
                kind: <StatefulSet as Resource>::KIND.to_owned(),
                name: t.stateful_set,
                container: t.container?,
            }),
            Target::Service(t) => Some(Self {
                api_version: <Service as Resource>::API_VERSION.to_owned(),
                kind: <Service as Resource>::KIND.to_owned(),
                name: t.service,
                container: t.container?,
            }),
            Target::ReplicaSet(t) => Some(Self {
                api_version: <ReplicaSet as Resource>::API_VERSION.to_owned(),
                kind: <ReplicaSet as Resource>::KIND.to_owned(),
                name: t.replica_set,
                container: t.container?,
            }),
            Target::Targetless => None,
        }
    }

    pub fn into_config(self) -> Option<Target> {
        format!(
            "{}/{}/container/{}",
            self.kind.to_ascii_lowercase(),
            self.name,
            self.container
        )
        .parse()
        .ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCiInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

#[derive(CustomResource, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterSession",
    status = "MirrordClusterSessionStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_metrics: Option<SessionJiraMetrics>,
    pub owner: SessionOwner,
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<SessionTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_info: Option<SessionCiInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_target: Option<SessionCopyTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_cluster_parent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<SessionHttpFilter>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionHttpFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_filter: Option<String>,
}

#[derive(CustomResource, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordSession",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordSessionSpec {
    pub owner: SessionOwner,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<SessionTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<SessionHttpFilter>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJiraMetrics {
    pub branch_name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCopyTarget {
    pub scaledown: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_timestamp: Option<MicroTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<SessionClosed>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionClosed {
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

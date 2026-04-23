use std::fmt;

use k8s_openapi::{
    Resource,
    api::{
        apps::v1::{Deployment, ReplicaSet, StatefulSet},
        batch::v1::{CronJob, Job},
        core::v1::{Pod, Service},
    },
};
use kube::CustomResource;
use mirrord_config::target::Target;
use mirrord_kube::api::kubernetes::rollout::Rollout;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

impl fmt::Display for SessionOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}@{}",
            self.username, self.k8s_username, self.hostname,
        )
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
    /// Create a [`SessionTarget`] from a [`Target`] with a resolved container.
    ///
    /// Returns `None` for [`Target::Targetless`] or if the [`Target`] doesn't have a container.
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

    /// Parse back into a [`Target`] by reconstructing the canonical target path string.
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

/// Public projection of an active mirrord session, intended for users discovering
/// joinable sessions. Served by the operator via API aggregation against
/// `operator.metalbear.co/v1/mirrordsessions`. Never persisted in etcd; the operator
/// computes the response from its internal session store on every request.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "MirrordSession",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordSessionSpec {
    pub owner: SessionOwner,

    pub key: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<SessionTarget>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<MirrordSessionHttpFilter>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordSessionHttpFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_filter: Option<String>,
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

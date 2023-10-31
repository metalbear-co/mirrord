use chrono::NaiveDate;
use kube::CustomResource;
use mirrord_config::target::{Target, TargetConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const TARGETLESS_TARGET_NAME: &str = "targetless";

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "Target",
    root = "TargetCrd",
    namespaced
)]
pub struct TargetSpec {
    /// None when targetless.
    pub target: Option<Target>,
    pub port_locks: Option<Vec<TargetPortLock>>,
}

impl TargetCrd {
    /// Creates target name in format of target_type.target_name.[container.container_name]
    /// for example:
    /// deploy.nginx
    /// deploy.nginx.container.nginx
    pub fn target_name(target: &Target) -> String {
        let (type_name, target, container) = match target {
            Target::Deployment(target) => ("deploy", &target.deployment, &target.container),
            Target::Pod(target) => ("pod", &target.pod, &target.container),
            Target::Rollout(target) => ("rollout", &target.rollout, &target.container),
            Target::Targetless => return TARGETLESS_TARGET_NAME.to_string(),
        };
        if let Some(container) = container {
            format!("{}.{}.container.{}", type_name, target, container)
        } else {
            format!("{}.{}", type_name, target)
        }
    }

    /// "targetless" ([`TARGETLESS_TARGET_NAME`]) if `None`,
    /// else <resource_type>.<resource_name>...
    pub fn target_name_by_config(target_config: &TargetConfig) -> String {
        target_config
            .path
            .as_ref()
            .map_or_else(|| TARGETLESS_TARGET_NAME.to_string(), Self::target_name)
    }

    pub fn name(&self) -> String {
        self.spec
            .target
            .as_ref()
            .map(Self::target_name)
            .unwrap_or(TARGETLESS_TARGET_NAME.to_string())
    }
}

impl From<TargetCrd> for TargetConfig {
    fn from(crd: TargetCrd) -> Self {
        TargetConfig {
            path: crd.spec.target,
            namespace: crd.metadata.namespace,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct TargetPortLock {
    pub target_hash: String,
    pub port: u16,
}

pub static OPERATOR_STATUS_NAME: &str = "operator";

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "MirrordOperator",
    root = "MirrordOperatorCrd",
    status = "MirrordOperatorStatus"
)]
pub struct MirrordOperatorSpec {
    pub operator_version: String,
    pub default_namespace: String,
    pub features: Option<Vec<OperatorFeatures>>,
    pub license: LicenseInfoOwned,
    pub protocol_version: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorStatus {
    pub sessions: Vec<Session>,
    pub statistics: Option<MirrordOperatorStatusStatistics>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorStatusStatistics {
    pub dau: usize,
    pub mau: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Session {
    pub id: Option<String>,
    pub duration_secs: u64,
    pub user: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LicenseInfoOwned {
    pub name: String,
    pub organization: String,
    pub expire_at: NaiveDate,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum OperatorFeatures {
    ProxyApi,
}

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
    struct = "TargetCrd",
    namespaced
)]
pub struct TargetSpec {
    /// None when targetless.
    pub target: Option<Target>,
}

impl TargetCrd {
    pub fn target_name(target: &Target) -> String {
        match target {
            Target::Deployment(target) => format!("deploy.{}", target.deployment),
            Target::Pod(target) => format!("pod.{}", target.pod),
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

pub static OPERATOR_STATUS_NAME: &str = "operator";

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "MirrordOperator",
    struct = "MirrordOperatorCrd",
    status = "MirrordOperatorStatus"
)]
pub struct MirrordOperatorSpec {
    pub operator_version: String,
    pub default_namespace: String,
    pub license: LicenseInfoOwned,
    pub telemetry_exports: Option<MirrordOperatorTelemetryReports>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorStatus {
    pub sessions: Vec<Session>,
    pub statistics: Option<MirrordOperatorStatusStatistics>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorTelemetryReports {
    id: String,
    reports: Vec<MirrordOperatorTelemetryReport>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorTelemetryReport {
    timestamp: u64,
    report: String,
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
    pub fingerprint: Option<String>,
}

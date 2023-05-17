use kube::CustomResource;
use mirrord_config::target::{Target, TargetConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::license::License;

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
    pub fn target_name_by_optional_config(target_config: &Option<TargetConfig>) -> String {
        target_config.as_ref().map_or_else(
            || TARGETLESS_TARGET_NAME.to_string(),
            |target_config| Self::target_name(&target_config.path),
        )
    }

    pub fn name(&self) -> String {
        self.spec
            .target
            .as_ref()
            .map(Self::target_name)
            .unwrap_or(TARGETLESS_TARGET_NAME.to_string())
    }

    pub fn from_target(target_config: TargetConfig) -> Option<Self> {
        let target = target_config.path;

        let target_name = Self::target_name(&target);

        let mut crd = TargetCrd::new(
            &target_name,
            TargetSpec {
                target: Some(target),
            },
        );

        crd.metadata.namespace = target_config.namespace;

        Some(crd)
    }
}

impl From<TargetCrd> for Option<TargetConfig> {
    fn from(crd: TargetCrd) -> Self {
        crd.spec.target.map(|target| TargetConfig {
            path: target,
            namespace: crd.metadata.namespace,
        })
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
    pub license: License,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorStatus {
    pub sessions: Vec<Session>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Session {
    pub id: Option<String>,
    pub duration_secs: u64,
    pub user: String,
    pub target: String,
}

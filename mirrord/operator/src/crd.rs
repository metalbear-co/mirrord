use kube::CustomResource;
use mirrord_config::target::{Target, TargetConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "Target",
    struct = "TargetCrd",
    namespaced
)]
pub struct TargetSpec {
    pub target: Target,
}

impl TargetCrd {
    pub fn target_name(target: &Target) -> String {
        match target {
            Target::Deployment(target) => format!("deploy.{}", target.deployment),
            Target::Pod(target) => format!("pod.{}", target.pod),
        }
    }

    pub fn name(&self) -> String {
        Self::target_name(&self.spec.target)
    }

    pub fn from_target(target_config: TargetConfig) -> Option<Self> {
        let target = target_config.path?;

        let target_name = match &target {
            Target::Deployment(target) => format!("deploy.{}", target.deployment),
            Target::Pod(target) => format!("pod.{}", target.pod),
        };

        let mut crd = TargetCrd::new(&target_name, TargetSpec { target });

        crd.metadata.namespace = target_config.namespace;

        Some(crd)
    }
}

impl From<TargetCrd> for TargetConfig {
    fn from(crd: TargetCrd) -> Self {
        TargetConfig {
            path: Some(crd.spec.target),
            namespace: crd.metadata.namespace,
        }
    }
}

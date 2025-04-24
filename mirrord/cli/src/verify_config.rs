//! `mirrord verify-config [--ide] {path}` builds a
//! [`VerifyConfig`](crate::Commands::VerifyConfig) enum after checking the config file passed in
//! `path`. It's used by the IDE plugins to display errors/warnings quickly, without having to start
//! mirrord-layer.
use error::CliResult;
use futures::TryFutureExt;
use mirrord_config::{
    config::ConfigContext,
    feature::FeatureConfig,
    target::{
        cron_job::CronJobTarget, deployment::DeploymentTarget, job::JobTarget, pod::PodTarget,
        replica_set::ReplicaSetTarget, rollout::RolloutTarget, service::ServiceTarget,
        stateful_set::StatefulSetTarget, workflow::WorkflowTarget, Target, TargetConfig,
    },
    LayerConfig,
};
use mirrord_progress::NullProgress;
use serde::Serialize;

use crate::{config::VerifyConfigArgs, error, CliError};

/// Practically the same as [`Target`], but differs in the way the `targetless` option is
/// serialized. [`Target::Targetless`] serializes as `null`, [`VerifiedTarget::Targetless`]
/// serializes as string `"targetless"`. This difference allows the IDEs to correctly decide whether
/// to show the target selection dialog.
///
/// Changing the way [`Target::Targetless`] serializes would be cumbersome for two reasons:
/// 1. It's used in a lot of places, e.g. CRDs
/// 2. `schemars` crate does not support nested `[serde(untagged)]` tags
#[derive(Serialize, Clone)]
enum VerifiedTarget {
    #[serde(rename = "targetless")]
    Targetless,

    #[serde(untagged)]
    Pod(PodTarget),
    #[serde(untagged)]
    Deployment(DeploymentTarget),
    #[serde(untagged)]
    Rollout(RolloutTarget),

    #[serde(untagged)]
    Job(JobTarget),

    #[serde(untagged)]
    CronJob(CronJobTarget),

    #[serde(untagged)]
    StatefulSet(StatefulSetTarget),

    #[serde(untagged)]
    Service(ServiceTarget),

    #[serde(untagged)]
    ReplicaSet(ReplicaSetTarget),

    #[serde(untagged)]
    Workflow(WorkflowTarget),
}

impl From<Target> for VerifiedTarget {
    fn from(value: Target) -> Self {
        match value {
            Target::Deployment(target) => Self::Deployment(target),
            Target::Pod(target) => Self::Pod(target),
            Target::Rollout(target) => Self::Rollout(target),
            Target::Job(target) => Self::Job(target),
            Target::CronJob(target) => Self::CronJob(target),
            Target::StatefulSet(target) => Self::StatefulSet(target),
            Target::Service(target) => Self::Service(target),
            Target::ReplicaSet(target) => Self::ReplicaSet(target),
            Target::Workflow(target) => Self::Workflow(target),
            Target::Targetless => Self::Targetless,
        }
    }
}

impl From<VerifiedTarget> for TargetType {
    fn from(value: VerifiedTarget) -> Self {
        match value {
            VerifiedTarget::Targetless => TargetType::Targetless,
            VerifiedTarget::Pod(_) => TargetType::Pod,
            VerifiedTarget::Deployment(_) => TargetType::Deployment,
            VerifiedTarget::Rollout(_) => TargetType::Rollout,
            VerifiedTarget::Job(_) => TargetType::Job,
            VerifiedTarget::CronJob(_) => TargetType::CronJob,
            VerifiedTarget::StatefulSet(_) => TargetType::StatefulSet,
            VerifiedTarget::Service(_) => TargetType::Service,
            VerifiedTarget::ReplicaSet(_) => TargetType::ReplicaSet,
            VerifiedTarget::Workflow(_) => TargetType::Workflow,
        }
    }
}

#[derive(Serialize, Clone)]
struct VerifiedTargetConfig {
    path: Option<VerifiedTarget>,
    namespace: Option<String>,
}

impl From<TargetConfig> for VerifiedTargetConfig {
    fn from(value: TargetConfig) -> Self {
        Self {
            path: value.path.map(Into::into),
            namespace: value.namespace,
        }
    }
}

/// Corresponds to variants of [`Target`].
#[derive(Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum TargetType {
    Targetless,
    Pod,
    Deployment,
    Rollout,
    Job,
    CronJob,
    StatefulSet,
    Service,
    ReplicaSet,
    Workflow,
}

impl core::fmt::Display for TargetType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let stringifed = match self {
            TargetType::Targetless => "targetless",
            TargetType::Pod => "pod",
            TargetType::Deployment => "deployment",
            TargetType::Rollout => "rollout",
            TargetType::Job => "job",
            TargetType::CronJob => "cronjob",
            TargetType::StatefulSet => "statefulset",
            TargetType::Service => "service",
            TargetType::ReplicaSet => "replicaset",
            TargetType::Workflow => "workflow",
        };

        f.write_str(stringifed)
    }
}

impl TargetType {
    fn all() -> impl Iterator<Item = Self> {
        [
            Self::Targetless,
            Self::Pod,
            Self::Deployment,
            Self::Rollout,
            Self::Job,
            Self::CronJob,
            Self::StatefulSet,
            Self::Service,
            Self::ReplicaSet,
            Self::Workflow,
        ]
        .into_iter()
    }

    fn compatible_with(&self, config: &FeatureConfig) -> bool {
        match self {
            Self::Targetless | Self::Rollout => !config.copy_target.enabled,
            Self::Pod => !(config.copy_target.enabled && config.copy_target.scale_down),
            Self::Job | Self::CronJob => config.copy_target.enabled,
            Self::Service => !config.copy_target.enabled,
            Self::Deployment | Self::StatefulSet | Self::ReplicaSet | Self::Workflow => true,
        }
    }
}

/// Produced by calling `verify_config`.
///
/// It's consumed by the IDEs to check if a config is valid, or missing something, without starting
/// mirrord fully.
#[derive(Serialize)]
#[serde(tag = "type")]
enum VerifiedConfig {
    /// mirrord is able to run with this config, but it might have some issues or weird behavior
    /// depending on the `warnings`.
    Success {
        /// A valid, verified config for the `target` part of mirrord.
        config: VerifiedTargetConfig,
        /// Improper combination of features was requested, but mirrord can still run.
        warnings: Vec<String>,
        /// Target types compatible with the source config.
        /// Meant to be used by IDE plugins for customizing target selection.
        compatible_target_types: Vec<TargetType>,
    },
    /// Invalid config was detected, mirrord cannot run.
    ///
    /// May be triggered by extra/lacking `,`, or invalid fields, etc.
    Fail { errors: Vec<String> },
}

/// Verifies a config file specified by `path`.
///
/// ## Usage
///
/// ```sh
/// mirrord verify-config [path]
/// ```
///
/// - Example:
///
/// ```sh
/// mirrord verify-config ./valid-config.json
///
///
/// {
///   "type": "Success",
///   "config": {
///     "path": {
///       "deployment": "sample-deployment",
///     },
///     "namespace": null
///   },
///   "warnings": [],
///   "compatible_target_types": ["targetless", "deployment", "rollout", "pod"]
/// }
/// ```
///
/// ```sh
/// mirrord verify-config ./broken-config.json
///
///
/// {
///   "type": "Fail",
///   "errors": ["mirrord-config: IO operation failed with `No such file or directory (os error 2)`"]
/// }
/// ```
pub(super) async fn verify_config(
    VerifyConfigArgs { ide, path }: VerifyConfigArgs,
) -> CliResult<()> {
    let mut config_context = ConfigContext::default()
        .empty_target_final(ide)
        .override_env(LayerConfig::FILE_PATH_ENV, path);

    let layer_config =
        std::future::ready(LayerConfig::resolve(&mut config_context).map_err(CliError::from))
            .and_then(|mut config| async {
                crate::profile::apply_profile_if_configured(&mut config, &NullProgress).await?;
                Ok(config)
            })
            .and_then(|config| async {
                config.verify(&mut config_context)?;
                Ok(config)
            })
            .await;

    let verified = match layer_config {
        Ok(config) => VerifiedConfig::Success {
            config: config.target.into(),
            warnings: config_context.into_warnings(),
            compatible_target_types: TargetType::all()
                .filter(|tt| tt.compatible_with(&config.feature))
                .collect(),
        },
        Err(fail) => VerifiedConfig::Fail {
            errors: vec![fail.to_string()],
        },
    };

    println!("{}", serde_json::to_string_pretty(&verified)?);

    Ok(())
}

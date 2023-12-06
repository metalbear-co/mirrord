//! `mirrord verify-config [--ide] {path}` builds a [`VerifyConfig`] enum after checking the
//! config file passed in `path`. It's used by the IDE plugins to display errors/warnings quickly,
//! without having to start mirrord-layer.
use error::Result;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    feature::FeatureConfig,
    target::{DeploymentTarget, PodTarget, RolloutTarget, Target, TargetConfig},
};
use serde::Serialize;

use crate::{config::VerifyConfigArgs, error, LayerFileConfig};

/// Practically the same as [`Target`], but differs in the way the `targetless` option is
/// serialized. [`Target::Targetless`] serializes as `null`, [`VerifiedTarget::Targetless`]
/// serializes as string `"targetless"`. This difference allows the IDEs to correctly decide whether
/// to show the target selection dialog.
///
/// Changing the way [`Target::Targetless`] serializes would be cumbersome for two reasons:
/// 1. It's used in a lot of places, e.g. CRDs
/// 2. `schemars` crate does not support nested `[serde(untagged)]` tags
#[derive(Serialize)]
enum VerifiedTarget {
    #[serde(rename = "targetless")]
    Targetless,
    #[serde(untagged)]
    Pod(PodTarget),
    #[serde(untagged)]
    Deployment(DeploymentTarget),
    #[serde(untagged)]
    Rollout(RolloutTarget),
}

impl From<Target> for VerifiedTarget {
    fn from(value: Target) -> Self {
        match value {
            Target::Deployment(d) => Self::Deployment(d),
            Target::Pod(p) => Self::Pod(p),
            Target::Rollout(r) => Self::Rollout(r),
            Target::Targetless => Self::Targetless,
        }
    }
}

#[derive(Serialize)]
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
}

impl TargetType {
    fn all() -> impl Iterator<Item = Self> {
        [Self::Targetless, Self::Pod, Self::Deployment, Self::Rollout].into_iter()
    }

    fn compatible_with(&self, config: &FeatureConfig) -> bool {
        match self {
            Self::Targetless | Self::Rollout => !config.copy_target.enabled,
            Self::Pod => !(config.copy_target.enabled && config.copy_target.scale_down),
            Self::Deployment => true,
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
pub(super) async fn verify_config(VerifyConfigArgs { ide, path }: VerifyConfigArgs) -> Result<()> {
    let mut config_context = ConfigContext::new(ide);

    let layer_config = LayerFileConfig::from_path(path)
        .and_then(|config| config.generate_config(&mut config_context))
        .and_then(|config| {
            config.verify(&mut config_context)?;
            Ok(config)
        });

    let verified = match layer_config {
        Ok(config) => VerifiedConfig::Success {
            config: config.target.into(),
            warnings: config_context.get_warnings().to_owned(),
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

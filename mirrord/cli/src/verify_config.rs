//! `mirrord verify-config [--ide] {path}` builds a [`VerifyConfig`] enum after checking the
//! config file passed in `path`. It's used by the IDE plugins to display errors/warnings quickly,
//! without having to start mirrord-layer.
use error::Result;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    target::TargetConfig,
};
use serde::{Deserialize, Serialize};

use crate::{config::VerifyConfigArgs, error, LayerFileConfig};

/// Produced by calling `verify_config`.
///
/// It's consumed by the IDEs to check if a config is valid, or missing something, without starting
/// mirrord fully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum VerifiedConfig {
    /// mirrord is able to run with this config, but it might have some issues or weird behavior
    /// depending on the `warnings`.
    Success {
        /// A valid, verified config for the `target` part of mirrord.
        config: TargetConfig,
        /// Improper combination of features was requested, but mirrord can still run.
        warnings: Vec<String>,
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
///   "warnings": []
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

    let verified = match LayerFileConfig::from_path(path)
        .and_then(|config| config.generate_config(&mut config_context))
        .and_then(|config| {
            config.verify(&mut config_context)?;
            Ok(config)
        }) {
        Ok(config) => VerifiedConfig::Success {
            config: config.target,
            warnings: config_context.get_warnings().to_owned(),
        },
        Err(fail) => VerifiedConfig::Fail {
            errors: vec![fail.to_string()],
        },
    };

    println!("{}", serde_json::to_string_pretty(&verified)?);

    Ok(())
}

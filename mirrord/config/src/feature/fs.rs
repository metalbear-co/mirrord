//! <!--${internal}-->
//! mirrord file operations support 2 modes of configuration:
//!
//! 1. [`FsUserConfig::Simple`]: controls only the option for enabling read-only, read-write,
//! or disable file operations;
//!
//! 2. [`FsUserConfig::Advanced`]: All of the above, plus allows setting up
//! [`mirrord_layer::file::filter::FileFilter`] to control which files should be opened
//! locally or remotely.
use schemars::JsonSchema;
use serde::Deserialize;

pub use self::{advanced::*, mode::*};
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::MirrordToggleableConfig,
};

pub mod advanced;
pub mod mode;

/// ## feature.fs {#fs}
///
/// Changes file operations behavior based on user configuration.
///
/// See the file operations [reference](https://mirrord.dev/docs/reference/fileops/)
/// for more details, and [fs advanced](#fs-advanced) for more information on how to fully setup
/// mirrord file operations.
///
/// ### Minimal `fs` config {#fs-minimal}
///
/// ```json
/// {
///   "feature": {
///     "fs": "read"
///   }
/// }
/// ```
///
/// ### Advanced `fs` config {#fs-advanced}
///
/// ```json
/// {
///   "feature": {
///     "fs": {
///       "mode": "write",
///       "read_write": ".+\.json" ,
///       "read_only": [ ".+\.yaml", ".+important-file\.txt" ],
///       "local": [ ".+\.js", ".+\.mjs" ]
///     }
///   }
/// }
/// ```
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "lowercase")]
pub enum FsUserConfig {
    /// <!--${internal}-->
    /// Basic configuration that controls the env vars `MIRRORD_FILE_OPS` and `MIRRORD_FILE_RO_OPS`
    /// (default).
    Simple(FsModeConfig),

    /// <!--${internal}-->
    /// Allows the user to specify both [`FsModeConfig`] (as above), and configuration for the
    /// overrides.
    Advanced(AdvancedFsUserConfig),
}

impl Default for FsUserConfig {
    fn default() -> Self {
        FsUserConfig::Simple(FsModeConfig::Read)
    }
}

impl MirrordConfig for FsUserConfig {
    type Generated = FsConfig;

    fn generate_config(self, warnings: &mut Vec<String>) -> Result<Self::Generated, ConfigError> {
        let config = match self {
            FsUserConfig::Simple(mode) => FsConfig {
                mode: mode.generate_config(warnings)?,
                read_write: FromEnv::new("MIRRORD_FILE_READ_WRITE_PATTERN")
                    .source_value(warnings)
                    .transpose()?,
                read_only: FromEnv::new("MIRRORD_FILE_READ_ONLY_PATTERN")
                    .source_value(warnings)
                    .transpose()?,
                local: FromEnv::new("MIRRORD_FILE_LOCAL_PATTERN")
                    .source_value(warnings)
                    .transpose()?,
                not_found: None,
            },
            FsUserConfig::Advanced(advanced) => advanced.generate_config(warnings)?,
        };

        Ok(config)
    }
}

impl MirrordToggleableConfig for FsUserConfig {
    fn disabled_config(warnings: &mut Vec<String>) -> Result<Self::Generated, ConfigError> {
        let mode = FsModeConfig::disabled_config(warnings)?;
        let read_write = FromEnv::new("MIRRORD_FILE_READ_WRITE_PATTERN")
            .source_value(warnings)
            .transpose()?;
        let read_only = FromEnv::new("MIRRORD_FILE_READ_ONLY_PATTERN")
            .source_value(warnings)
            .transpose()?;
        let local = FromEnv::new("MIRRORD_FILE_LOCAL_PATTERN")
            .source_value(warnings)
            .transpose()?;

        Ok(FsConfig {
            mode,
            read_write,
            read_only,
            local,
            not_found: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::config::MirrordConfig;

    #[rstest]
    fn fs_config_default() {
        let mut warnings = Vec::new();
        let expect = FsConfig {
            mode: FsModeConfig::Read,
            ..Default::default()
        };

        let fs_config = FsUserConfig::default()
            .generate_config(&mut warnings)
            .unwrap();

        assert_eq!(fs_config, expect);
    }
}

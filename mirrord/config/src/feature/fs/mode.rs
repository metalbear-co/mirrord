use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        from_env::FromEnv, source::MirrordConfigSource, ConfigError, FromMirrordConfig,
        MirrordConfig, Result,
    },
    util::MirrordToggleableConfig,
};

/// Configuration for enabling read-only or read-write file operations.
///
/// These options are overriden by user specified overrides and mirrord default overrides.
///
/// If you set [`"localwithoverrides"`](#feature-fs-mode-localwithoverrides) then some files
/// can be read/write remotely based on our default/user specified.
/// Default option for general file configuration.
///
/// The accepted values are: `"local"`, `"localwithoverrides`, `"read"`, or `"write`.
#[derive(Serialize, Deserialize, Default, PartialEq, Eq, Clone, Debug, Copy, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FsModeConfig {
    /// #### feature.fs.mode.local {#feature-fs-mode-local}
    ///
    /// mirrord won't do anything fs-related, all operations will be local.
    Local,

    /// #### feature.fs.mode.localwithoverrides {#feature-fs-mode-localwithoverrides}
    ///
    /// mirrord will run overrides on some file operations, but most will be local.
    LocalWithOverrides,

    /// #### feature.fs.mode.read {#feature-fs-mode-read}
    ///
    /// mirrord will read files from the remote, but won't write to them.
    #[default]
    Read,

    /// #### feature.fs.mode.write {#feature-fs-mode-write}
    ///
    /// mirrord will read/write from the remote.
    Write,
}

impl FsModeConfig {
    pub fn is_local(self) -> bool {
        matches!(self, FsModeConfig::Local)
    }

    pub fn is_read(self) -> bool {
        matches!(self, FsModeConfig::Read | FsModeConfig::LocalWithOverrides)
    }

    pub fn is_write(self) -> bool {
        self == FsModeConfig::Write
    }
}

impl FromStr for FsModeConfig {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "local" => Ok(FsModeConfig::Local),
            "localwithoverrides" => Ok(FsModeConfig::LocalWithOverrides),
            "read" => Ok(FsModeConfig::Read),
            "write" => Ok(FsModeConfig::Write),
            _ => Err(ConfigError::InvalidFsMode(s.to_string())),
        }
    }
}

impl FsModeConfig {
    fn from_env_logic(fs: Option<bool>, ro_fs: Option<bool>) -> Option<Self> {
        match (fs, ro_fs) {
            (Some(false), Some(true)) | (None, Some(true)) => Some(FsModeConfig::Read),
            (Some(true), _) => Some(FsModeConfig::Write),
            (Some(false), Some(false)) | (None, Some(false)) | (Some(false), None) => {
                Some(FsModeConfig::Local)
            }
            (None, None) => None,
        }
    }
}

impl MirrordConfig for FsModeConfig {
    type Generated = FsModeConfig;

    fn generate_config(self) -> Result<Self::Generated> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS")
            .source_value()
            .transpose()?;
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS")
            .source_value()
            .transpose()?;
        let mode = FromEnv::new("MIRRORD_FILE_MODE")
            .source_value()
            .transpose()?;

        if let Some(mode) = mode {
            Ok(mode)
        } else {
            Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(self))
        }
    }
}

impl MirrordToggleableConfig for FsModeConfig {
    fn disabled_config() -> Result<Self::Generated> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS")
            .source_value()
            .transpose()?;
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS")
            .source_value()
            .transpose()?;
        let mode = FromEnv::new("MIRRORD_FILE_MODE")
            .source_value()
            .transpose()?;
        if let Some(mode) = mode {
            Ok(mode)
        } else {
            Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(FsModeConfig::Local))
        }
    }
}

impl FromMirrordConfig for FsModeConfig {
    type Generator = FsModeConfig;
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        util::{testing::with_env_vars, ToggleableConfig},
    };

    #[rstest]
    #[case(None, None, FsModeConfig::Read)]
    #[case(Some("true"), None, FsModeConfig::Write)]
    #[case(Some("true"), Some("true"), FsModeConfig::Write)]
    #[case(Some("false"), Some("true"), FsModeConfig::Read)]
    fn default(#[case] fs: Option<&str>, #[case] ro: Option<&str>, #[case] expect: FsModeConfig) {
        with_env_vars(
            vec![("MIRRORD_FILE_OPS", fs), ("MIRRORD_FILE_RO_OPS", ro)],
            || {
                let fs = FsModeConfig::default().generate_config().unwrap();

                assert_eq!(fs, expect);
            },
        );
    }

    #[rstest]
    #[case(None, None, FsModeConfig::Local)]
    #[case(Some("true"), None, FsModeConfig::Write)]
    #[case(Some("true"), Some("true"), FsModeConfig::Write)]
    #[case(Some("false"), Some("true"), FsModeConfig::Read)]
    fn disabled(#[case] fs: Option<&str>, #[case] ro: Option<&str>, #[case] expect: FsModeConfig) {
        with_env_vars(
            vec![("MIRRORD_FILE_OPS", fs), ("MIRRORD_FILE_RO_OPS", ro)],
            || {
                let fs = ToggleableConfig::<FsModeConfig>::Enabled(false)
                    .generate_config()
                    .unwrap();

                assert_eq!(fs, expect);
            },
        );
    }
}

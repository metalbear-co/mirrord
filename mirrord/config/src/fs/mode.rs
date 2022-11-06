use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::MirrordToggleableConfig,
};

/// Configuration for enabling read-only and read-write file operations.
///
/// Default option for general file configuration. Allows the user to specify:
///
/// - `MIRRORD_FILE_OPS` and `MIRRORD_FILE_RO_OPS`;
///
/// ## Examples
///
/// - Disable mirrord file operations:
///
/// ```yaml
/// # mirrord-config.yaml
///
/// fs = disabled
/// ```
///
/// - Enable mirrord read-write file operations:
///
/// ```yaml
/// # mirrord-config.yaml
///
/// fs = write
/// ```
#[derive(Deserialize, Default, PartialEq, Eq, Clone, Debug, Copy, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FsModeConfig {
    Disabled,
    #[default]
    Read,
    Write,
}

impl FsModeConfig {
    pub fn is_read(self) -> bool {
        self == FsModeConfig::Read
    }

    pub fn is_write(self) -> bool {
        self == FsModeConfig::Write
    }
}

impl FsModeConfig {
    fn from_env_logic(fs: Option<bool>, ro_fs: Option<bool>) -> Option<Self> {
        match (fs, ro_fs) {
            (Some(false), Some(true)) | (None, Some(true)) => Some(FsModeConfig::Read),
            (Some(true), _) => Some(FsModeConfig::Write),
            (Some(false), Some(false)) | (None, Some(false)) | (Some(false), None) => {
                Some(FsModeConfig::Disabled)
            }
            (None, None) => None,
        }
    }
}

impl MirrordConfig for FsModeConfig {
    type Generated = FsModeConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS").source_value();
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();

        Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(self))
    }
}

impl MirrordToggleableConfig for FsModeConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS").source_value();
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();

        Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(FsModeConfig::Disabled))
    }
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
    #[case(None, None, FsModeConfig::Disabled)]
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

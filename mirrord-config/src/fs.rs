use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::MirrordToggleableConfig,
};

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FsConfig {
    Disabled,
    Read,
    Write,
}

impl FsConfig {
    pub fn is_read(&self) -> bool {
        self == &FsConfig::Read
    }

    pub fn is_write(&self) -> bool {
        self == &FsConfig::Write
    }
}

impl Default for FsConfig {
    fn default() -> Self {
        FsConfig::Read
    }
}

impl FsConfig {
    fn from_env_logic(fs: Option<bool>, ro_fs: Option<bool>) -> Option<Self> {
        match (fs, ro_fs) {
            (Some(false), Some(true)) | (None, Some(true)) => Some(FsConfig::Read),
            (Some(true), _) => Some(FsConfig::Write),
            (Some(false), Some(false)) | (None, Some(false)) | (Some(false), None) => {
                Some(FsConfig::Disabled)
            }
            (None, None) => None,
        }
    }
}

impl MirrordConfig for FsConfig {
    type Generated = FsConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS").source_value();
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();

        Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(self))
    }
}

impl MirrordToggleableConfig for FsConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS").source_value();
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();

        Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(FsConfig::Disabled))
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
    #[case(None, None, FsConfig::Read)]
    #[case(Some("true"), None, FsConfig::Write)]
    #[case(Some("true"), Some("true"), FsConfig::Write)]
    #[case(Some("false"), Some("true"), FsConfig::Read)]
    fn default(#[case] fs: Option<&str>, #[case] ro: Option<&str>, #[case] expect: FsConfig) {
        with_env_vars(
            vec![("MIRRORD_FILE_OPS", fs), ("MIRRORD_FILE_RO_OPS", ro)],
            || {
                let fs = FsConfig::default().generate_config().unwrap();

                assert_eq!(fs, expect);
            },
        );
    }

    #[rstest]
    #[case(None, None, FsConfig::Disabled)]
    #[case(Some("true"), None, FsConfig::Write)]
    #[case(Some("true"), Some("true"), FsConfig::Write)]
    #[case(Some("false"), Some("true"), FsConfig::Read)]
    fn disabled(#[case] fs: Option<&str>, #[case] ro: Option<&str>, #[case] expect: FsConfig) {
        with_env_vars(
            vec![("MIRRORD_FILE_OPS", fs), ("MIRRORD_FILE_RO_OPS", ro)],
            || {
                let fs = ToggleableConfig::<FsConfig>::Enabled(false)
                    .generate_config()
                    .unwrap();

                assert_eq!(fs, expect);
            },
        );
    }
}

use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::MirrordFlaggedConfig,
};

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum FsField {
    Disabled,
    Read,
    Write,
}

impl FsField {
    pub fn is_read(&self) -> bool {
        self == &FsField::Read
    }

    pub fn is_write(&self) -> bool {
        self == &FsField::Write
    }
}

impl Default for FsField {
    fn default() -> Self {
        FsField::Read
    }
}

impl FsField {
    fn from_env_logic(fs: Option<bool>, ro_fs: Option<bool>) -> Option<Self> {
        match (fs, ro_fs) {
            (Some(false), Some(true)) | (None, Some(true)) => Some(FsField::Read),
            (Some(true), _) => Some(FsField::Write),
            (Some(false), Some(false)) | (None, Some(false)) | (Some(false), None) => {
                Some(FsField::Disabled)
            }
            (None, None) => None,
        }
    }
}

impl MirrordConfig for FsField {
    type Generated = FsField;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS").source_value();
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();

        Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(self))
    }
}

impl MirrordFlaggedConfig for FsField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let fs = FromEnv::new("MIRRORD_FILE_OPS").source_value();
        let ro_fs = FromEnv::new("MIRRORD_FILE_RO_OPS").source_value();

        Ok(Self::from_env_logic(fs, ro_fs).unwrap_or(FsField::Disabled))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        util::{testing::with_env_vars, FlagField},
    };

    #[rstest]
    #[case(None, None, FsField::Read)]
    #[case(Some("true"), None, FsField::Write)]
    #[case(Some("true"), Some("true"), FsField::Write)]
    #[case(Some("false"), Some("true"), FsField::Read)]
    fn default(#[case] fs: Option<&str>, #[case] ro: Option<&str>, #[case] expect: FsField) {
        with_env_vars(
            vec![("MIRRORD_FILE_OPS", fs), ("MIRRORD_FILE_RO_OPS", ro)],
            || {
                let fs = FsField::default().generate_config().unwrap();

                assert_eq!(fs, expect);
            },
        );
    }

    #[rstest]
    #[case(None, None, FsField::Disabled)]
    #[case(Some("true"), None, FsField::Write)]
    #[case(Some("true"), Some("true"), FsField::Write)]
    #[case(Some("false"), Some("true"), FsField::Read)]
    fn disabled(#[case] fs: Option<&str>, #[case] ro: Option<&str>, #[case] expect: FsField) {
        with_env_vars(
            vec![("MIRRORD_FILE_OPS", fs), ("MIRRORD_FILE_RO_OPS", ro)],
            || {
                let fs = FlagField::<FsField>::Enabled(false)
                    .generate_config()
                    .unwrap();

                assert_eq!(fs, expect);
            },
        );
    }
}

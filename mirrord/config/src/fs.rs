use schemars::JsonSchema;
/// mirrord file operations support 2 modes of configuration:
///
/// 1. [`FsUserConfig::Simple`]: controls only the option for enabling read-only, read-write,
/// or disable file operations;
///
/// 2. [`FsUserConfig::Advanced`]: All of the above, plus allows setting up
/// [`mirrord_layer::file::filter::FileFilter`] to control which files should be opened
/// locally or remotely.
use serde::Deserialize;

pub use self::{advanced::*, mode::*};
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig},
    util::MirrordToggleableConfig,
};

pub mod advanced;
pub mod mode;

/// Changes file operations behavior based on user configuration.
///
/// Defaults to [`FsUserConfig::Simple`], with [`FsModeConfig::Read`].
///
/// See the file operations [reference](https://mirrord.dev/docs/reference/fileops/)
/// for more details.
///
/// ## Examples
///
/// - Read-write file operations:
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature]
/// fs = "write"
/// ```
/// - Read `/lib` locally, `/etc` remotely and `/var/run` read write remotely. Rest local
///
/// ```yaml
/// # mirrord-config.yaml
///
/// [fs]
/// mode = read
/// read_write = ["/var/run"]
/// read_only = ["/etc"]
/// local = ["/lib"]
/// ```
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "lowercase")]
pub enum FsUserConfig {
    /// Basic configuration that controls the env vars `MIRRORD_FILE_OPS` and `MIRRORD_FILE_RO_OPS`
    /// (default).
    Simple(FsModeConfig),

    /// Allows the user to specify both [`FsModeConfig`] (as above), and configuration for the
    /// `MIRRORD_FILE_FILTER_INCLUDE` and `MIRRORD_FILE_FILTER_EXCLUDE` env vars.
    Advanced(AdvancedFsUserConfig),
}

impl Default for FsUserConfig {
    fn default() -> Self {
        FsUserConfig::Simple(FsModeConfig::Read)
    }
}

impl MirrordConfig for FsUserConfig {
    type Generated = FsConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let config = match self {
            FsUserConfig::Simple(mode) => FsConfig {
                mode: mode.generate_config()?,
                include: FromEnv::new("MIRRORD_FILE_FILTER_INCLUDE")
                    .source_value()
                    .transpose()?,
                exclude: FromEnv::new("MIRRORD_FILE_FILTER_EXCLUDE")
                    .source_value()
                    .transpose()?,
                read_write: FromEnv::new("MIRRORD_FILE_READ_WRITE_PATTERN")
                    .source_value()
                    .transpose()?,
                read_only: FromEnv::new("MIRRORD_FILE_READ_ONLY_PATTERN")
                    .source_value()
                    .transpose()?,
                local: FromEnv::new("MIRRORD_FILE_LOCAL_PATTERN")
                    .source_value()
                    .transpose()?,
            },
            FsUserConfig::Advanced(advanced) => advanced.generate_config()?,
        };

        Ok(config)
    }
}

impl MirrordToggleableConfig for FsUserConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let mode = FsModeConfig::disabled_config()?;
        let include = FromEnv::new("MIRRORD_FILE_FILTER_INCLUDE")
            .source_value()
            .transpose()?;
        let exclude = FromEnv::new("MIRRORD_FILE_FILTER_EXCLUDE")
            .source_value()
            .transpose()?;
        let read_write = FromEnv::new("MIRRORD_FILE_READ_WRITE_PATTERN")
            .source_value()
            .transpose()?;
        let read_only = FromEnv::new("MIRRORD_FILE_READ_ONLY_PATTERN")
            .source_value()
            .transpose()?;
        let local = FromEnv::new("MIRRORD_FILE_LOCAL_PATTERN")
            .source_value()
            .transpose()?;

        Ok(FsConfig {
            mode,
            include,
            exclude,
            read_write,
            read_only,
            local,
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        util::{testing::with_env_vars, VecOrSingle},
    };

    #[rstest]
    fn test_fs_config_default() {
        let expect = FsConfig {
            mode: FsModeConfig::Read,
            ..Default::default()
        };

        with_env_vars(
            vec![
                ("MIRRORD_FILE_RO_OPS", Some("true")),
                ("MIRRORD_FILE_OPS", None),
                ("MIRRORD_FILE_FILTER_INCLUDE", None),
                ("MIRRORD_FILE_FILTER_EXCLUDE", None),
            ],
            || {
                let fs_config = FsUserConfig::default().generate_config().unwrap();

                assert_eq!(fs_config, expect);
            },
        );
    }

    #[rstest]
    fn test_fs_config_read_only_include() {
        let expect = FsConfig {
            mode: FsModeConfig::Read,
            include: Some(VecOrSingle::Single(".*".to_string())),
            exclude: None,
            read_write: None,
            read_only: None,
            local: None,
        };

        with_env_vars(
            vec![
                ("MIRRORD_FILE_RO_OPS", Some("true")),
                ("MIRRORD_FILE_OPS", None),
                ("MIRRORD_FILE_FILTER_INCLUDE", Some(".*")),
                ("MIRRORD_FILE_FILTER_EXCLUDE", None),
            ],
            || {
                let fs_config = FsUserConfig::Advanced(AdvancedFsUserConfig {
                    mode: Some(FsModeConfig::Read),
                    include: Some(VecOrSingle::Single(".*".to_string())),
                    exclude: None,
                    read_write: None,
                    read_only: None,
                    local: None,
                })
                .generate_config()
                .unwrap();

                assert_eq!(fs_config, expect);
            },
        );
    }

    #[rstest]
    fn test_fs_config_read_only_exclude() {
        let expect = FsConfig {
            mode: FsModeConfig::Read,
            include: None,
            exclude: Some(VecOrSingle::Single(".*".to_string())),
            read_write: None,
            read_only: None,
            local: None,
        };

        with_env_vars(
            vec![
                ("MIRRORD_FILE_RO_OPS", Some("true")),
                ("MIRRORD_FILE_OPS", None),
                ("MIRRORD_FILE_FILTER_INCLUDE", None),
                ("MIRRORD_FILE_FILTER_EXCLUDE", Some(".*")),
            ],
            || {
                let fs_config = FsUserConfig::Advanced(AdvancedFsUserConfig {
                    mode: Some(FsModeConfig::Read),
                    include: None,
                    exclude: Some(VecOrSingle::Single(".*".to_string())),
                    local: None,
                    read_only: None,
                    read_write: None,
                })
                .generate_config()
                .unwrap();

                assert_eq!(fs_config, expect);
            },
        );
    }

    #[rstest]
    fn test_fs_config_read_only_include_exclude() {
        let expect = FsConfig {
            mode: FsModeConfig::Read,
            include: Some(VecOrSingle::Single(".*".to_string())),
            exclude: Some(VecOrSingle::Single(".*".to_string())),
            read_write: None,
            read_only: None,
            local: None,
        };

        with_env_vars(
            vec![
                ("MIRRORD_FILE_RO_OPS", Some("true")),
                ("MIRRORD_FILE_OPS", None),
                ("MIRRORD_FILE_FILTER_INCLUDE", Some(".*")),
                ("MIRRORD_FILE_FILTER_EXCLUDE", Some(".*")),
            ],
            || {
                let fs_config = FsUserConfig::Advanced(AdvancedFsUserConfig {
                    mode: Some(FsModeConfig::Read),
                    include: Some(VecOrSingle::Single(".*".to_string())),
                    exclude: Some(VecOrSingle::Single(".*".to_string())),
                    read_only: None,
                    read_write: None,
                    local: None,
                })
                .generate_config()
                .unwrap();

                assert_eq!(fs_config, expect);
            },
        );
    }

    #[rstest]
    fn test_fs_config_read_write_include_exclude() {
        let expect = FsConfig {
            mode: FsModeConfig::Write,
            include: Some(VecOrSingle::Single(".*".to_string())),
            exclude: Some(VecOrSingle::Single(".*".to_string())),
            read_only: None,
            read_write: None,
            local: None,
        };

        with_env_vars(
            vec![
                ("MIRRORD_FILE_RO_OPS", None),
                ("MIRRORD_FILE_OPS", Some("true")),
                ("MIRRORD_FILE_FILTER_INCLUDE", Some(".*")),
                ("MIRRORD_FILE_FILTER_EXCLUDE", Some(".*")),
            ],
            || {
                let fs_config = FsUserConfig::Advanced(AdvancedFsUserConfig {
                    mode: Some(FsModeConfig::Write),
                    include: Some(VecOrSingle::Single(".*".to_string())),
                    exclude: Some(VecOrSingle::Single(".*".to_string())),
                    read_only: None,
                    read_write: None,
                    local: None,
                })
                .generate_config()
                .unwrap();

                assert_eq!(fs_config, expect);
            },
        );
    }
}

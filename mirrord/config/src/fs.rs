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
///
/// - Read-only excluding `.foo` files:
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.fs]
/// mode = "read"
/// exclude = "^.*\.foo$"
/// ```
///
/// - Read-write including only `.baz` files:
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.fs]
/// mode = "write"
/// include = "^.*\.baz$"
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
                include: FromEnv::new("MIRRORD_FILE_FILTER_INCLUDE").source_value(),
                exclude: FromEnv::new("MIRRORD_FILE_FILTER_EXCLUDE").source_value(),
            },
            FsUserConfig::Advanced(advanced) => advanced.generate_config()?,
        };

        Ok(config)
    }
}

impl MirrordToggleableConfig for FsUserConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let mode = FsModeConfig::disabled_config()?;
        let include = FromEnv::new("MIRRORD_FILE_FILTER_INCLUDE").source_value();
        let exclude = FromEnv::new("MIRRORD_FILE_FILTER_EXCLUDE").source_value();

        Ok(FsConfig {
            mode,
            include,
            exclude,
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
                    mode: FsModeConfig::Read,
                    include: Some(VecOrSingle::Single(".*".to_string())),
                    exclude: None,
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
                    mode: FsModeConfig::Read,
                    include: None,
                    exclude: Some(VecOrSingle::Single(".*".to_string())),
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
                    mode: FsModeConfig::Read,
                    include: Some(VecOrSingle::Single(".*".to_string())),
                    exclude: Some(VecOrSingle::Single(".*".to_string())),
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
                    mode: FsModeConfig::Write,
                    include: Some(VecOrSingle::Single(".*".to_string())),
                    exclude: Some(VecOrSingle::Single(".*".to_string())),
                })
                .generate_config()
                .unwrap();

                assert_eq!(fs_config, expect);
            },
        );
    }
}

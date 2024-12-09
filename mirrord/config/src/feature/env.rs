use std::{collections::HashMap, path::PathBuf};

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, Result},
    util::{MirrordToggleableConfig, VecOrSingle},
};

pub mod mapper;

pub const MIRRORD_OVERRIDE_ENV_VARS_INCLUDE_ENV: &str = "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE";
pub const MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE_ENV: &str = "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE";
pub const MIRRORD_OVERRIDE_ENV_FILE_ENV: &str = "MIRRORD_OVERRIDE_ENV_VARS_FILE";

/// Allows the user to set or override the local process' environment variables with the ones
/// from the remote pod.
///
/// Can be set to one of the options:
///
/// 1. `false` - Disables the feature, won't have remote environment variables.
/// 2. `true` - Enables the feature, will obtain remote environment variables.
/// 3. object - see below (means `true` + additional configuration).
///
/// Which environment variables to load from the remote pod are controlled by setting either
/// [`include`](#feature-env-include) or [`exclude`](#feature-env-exclude).
///
/// See the environment variables [reference](https://mirrord.dev/docs/reference/env/) for more details.
///
/// ```json
/// {
///   "feature": {
///     "env": {
///       "include": "DATABASE_USER;PUBLIC_ENV;MY_APP_*",
///       "exclude": "DATABASE_PASSWORD;SECRET_ENV",
///       "override": {
///         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
///         "LOCAL_BEAR": "panda"
///       }
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize)]
#[config(map_to = "EnvFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct EnvConfig {
    /// ### feature.env.include {#feature-env-include}
    ///
    /// Include only these remote environment variables in the local process.
    /// Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
    /// any character and `*` matches arbitrary many (including zero) occurrences of any character.
    ///
    /// Can be passed as a list or as a semicolon-delimited string (e.g. `"VAR;OTHER_VAR"`).
    ///
    /// Some environment variables are excluded by default (`PATH` for example), including these
    /// requires specifying them with `include`
    #[config(env = MIRRORD_OVERRIDE_ENV_VARS_INCLUDE_ENV)]
    pub include: Option<VecOrSingle<String>>,

    /// ### feature.env.exclude {#feature-env-exclude}
    ///
    /// Include the remote environment variables in the local process that are **NOT** specified by
    /// this option.
    /// Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
    /// any character and `*` matches arbitrary many (including zero) occurrences of any character.
    ///
    /// Some of the variables that are excluded by default:
    /// `PATH`, `HOME`, `HOMEPATH`, `CLASSPATH`, `JAVA_EXE`, `JAVA_HOME`, `PYTHONPATH`.
    ///
    /// Can be passed as a list or as a semicolon-delimited string (e.g. `"VAR;OTHER_VAR"`).
    #[config(env = MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE_ENV)]
    pub exclude: Option<VecOrSingle<String>>,

    /// ### feature.env.override {#feature-env-override}
    ///
    /// Allows setting or overriding environment variables (locally) with a custom value.
    ///
    /// For example, if the remote pod has an environment variable `REGION=1`, but this is an
    /// undesirable value, it's possible to use `override` to set `REGION=2` (locally) instead.
    ///
    /// Environment specified here will also override variables passed via the env file.
    pub r#override: Option<HashMap<String, String>>, // `r#`: `override` is a Rust keyword.

    /// ### feature.env.load_from_process {#feature-env-load_from_process}
    ///
    /// Allows for changing the way mirrord loads remote environment variables.
    /// If set, the variables are fetched after the user application is started.
    ///
    /// This setting is meant to resolve issues when using mirrord via the IntelliJ plugin on WSL
    /// and the remote environment contains a lot of variables.
    pub load_from_process: Option<bool>,

    /// ### feature.env.unset {#feature-env-unset}
    ///
    /// Allows unsetting environment variables in the executed process.
    ///
    /// This is useful for when some system/user-defined environment like `AWS_PROFILE` make the
    /// application behave as if it's running locally, instead of using the remote settings.
    /// The unsetting happens from extension (if possible)/CLI and when process initializes.
    /// In some cases, such as Go the env might not be able to be modified from the process itself.
    /// This is case insensitive, meaning if you'd put `AWS_PROFILE` it'd unset both `AWS_PROFILE`
    /// and `Aws_Profile` and other variations.
    pub unset: Option<VecOrSingle<String>>,

    /// ### feature.env_file {#feature-env-file}
    ///
    /// Allows for passing environment variables from an env file.
    ///
    /// These variables will override environment fetched from the remote target.
    #[config(env = MIRRORD_OVERRIDE_ENV_FILE_ENV)]
    pub env_file: Option<PathBuf>,

    /// ### feature.env.mapping {#feature-env-mapping}
    ///
    /// Specify map of patterns that if matched will replace the value according to specification.
    ///
    /// *Capture groups are allowed.*
    ///
    /// Example:
    /// ```json
    /// {
    ///   ".+_TIMEOUT": "10000"
    ///   "LOG_.+_VERBOSITY": "debug"
    ///   "(\w+)_(\d+)": "magic-value"
    /// }
    /// ```
    ///
    /// Will do the next replacements for environment variables that match:
    ///
    /// `CONNECTION_TIMEOUT: 500` => `CONNECTION_TIMEOUT: 10000`
    /// `LOG_FILE_VERBOSITY: info` => `LOG_FILE_VERBOSITY: debug`
    /// `DATA_1234: common-value` => `DATA_1234: magic-value`
    pub mapping: Option<HashMap<String, String>>,
}

impl MirrordToggleableConfig for EnvFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated> {
        Ok(EnvConfig {
            include: FromEnv::new(MIRRORD_OVERRIDE_ENV_VARS_INCLUDE_ENV)
                .source_value(context)
                .transpose()?,
            exclude: FromEnv::new(MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE_ENV)
                .source_value(context)
                .transpose()?
                .or_else(|| Some(VecOrSingle::Single("*".to_owned()))),
            load_from_process: None,
            r#override: None,
            unset: None,
            env_file: FromEnv::new(MIRRORD_OVERRIDE_ENV_FILE_ENV)
                .source_value(context)
                .transpose()?,
            mapping: None,
        })
    }
}

impl CollectAnalytics for &EnvConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add(
            "include_count",
            self.include
                .as_ref()
                .map(|v| v.len() as u32)
                .unwrap_or_default(),
        );
        analytics.add(
            "exclude_count",
            self.exclude
                .as_ref()
                .map(|v| v.len() as u32)
                .unwrap_or_default(),
        );
        analytics.add(
            "overrides_count",
            self.r#override
                .as_ref()
                .map(|v| v.len() as u32)
                .unwrap_or_default(),
        );
        analytics.add(
            "unset_count",
            self.unset
                .as_ref()
                .map(|v| v.len() as u32)
                .unwrap_or_default(),
        );
        analytics.add("env_file_used", self.env_file.is_some());
        analytics.add(
            "env_mapping_count",
            self.mapping
                .as_ref()
                .map(|v| v.len() as u32)
                .unwrap_or_default(),
        );
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
    fn default(
        #[values((None, None), (Some("IVAR1"), Some("IVAR1")), (Some("IVAR1;IVAR2"), Some("IVAR1;IVAR2")))]
        include: (Option<&str>, Option<&str>),
        #[values((None, None), (Some("EVAR1"), Some("EVAR1")))] exclude: (
            Option<&str>,
            Option<&str>,
        ),
    ) {
        with_env_vars(
            vec![
                (MIRRORD_OVERRIDE_ENV_VARS_INCLUDE_ENV, include.0),
                (MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE_ENV, exclude.0),
            ],
            || {
                let mut cfg_context = ConfigContext::default();
                let env = EnvFileConfig::default()
                    .generate_config(&mut cfg_context)
                    .unwrap();

                assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
                assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
            },
        );
    }

    #[rstest]
    fn disabled_config(
        #[values((None, None), (Some("IVAR1"), Some("IVAR1")))] include: (
            Option<&str>,
            Option<&str>,
        ),
        #[values((None, Some("*")), (Some("EVAR1"), Some("EVAR1")))] exclude: (
            Option<&str>,
            Option<&str>,
        ),
    ) {
        with_env_vars(
            vec![
                (MIRRORD_OVERRIDE_ENV_VARS_INCLUDE_ENV, include.0),
                (MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE_ENV, exclude.0),
            ],
            || {
                let mut cfg_context = ConfigContext::default();
                let env = ToggleableConfig::<EnvFileConfig>::Enabled(false)
                    .generate_config(&mut cfg_context)
                    .unwrap();

                assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
                assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
            },
        );
    }
}

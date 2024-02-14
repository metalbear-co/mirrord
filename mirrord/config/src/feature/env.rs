use std::collections::HashMap;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, Result},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// Allows the user to set or override the local process' environment variables with the ones
/// from the remote pod.
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
#[derive(MirrordConfig, Clone, Debug)]
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
    #[config(env = "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")]
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
    #[config(env = "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")]
    pub exclude: Option<VecOrSingle<String>>,

    /// ### feature.env.override {#feature-env-override}
    ///
    /// Allows setting or overriding environment variables (locally) with a custom value.
    ///
    /// For example, if the remote pod has an environment variable `REGION=1`, but this is an
    /// undesirable value, it's possible to use `override` to set `REGION=2` (locally) instead.
    pub r#override: Option<HashMap<String, String>>, // `r#`: `override` is a Rust keyword.

    /// ### feature.env.load_from_process {#feature-env-load_from_process}
    ///
    /// Allows for changing the way mirrord loads remote environment variables.
    /// If set, the variables are fetched after the user application is started.
    ///
    /// This setting is meant to resolve issues when using mirrord via the IntelliJ plugin on WSL
    /// and the remote environment contains a lot of variables.
    pub load_from_process: Option<bool>,
}

impl MirrordToggleableConfig for EnvFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated> {
        Ok(EnvConfig {
            include: FromEnv::new("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")
                .source_value(context)
                .transpose()?,
            exclude: FromEnv::new("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")
                .source_value(context)
                .transpose()?
                .or_else(|| Some(VecOrSingle::Single("*".to_owned()))),
            load_from_process: None,
            r#override: None,
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
                ("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE", include.0),
                ("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE", exclude.0),
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
                ("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE", include.0),
                ("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE", exclude.0),
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

use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

const INCLUDE: &str = "MIRRORD_FILE_PATH_INCLUDE";
const EXCLUDE: &str = "MIRRORD_FILE_PATH_EXCLUDE";

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = FileFilterConfig)]
pub struct FileFilterOption {
    // TODO(alex) [low] 2022-10-14: It would be nice if we could set `env = GLOBAL` to avoid
    // repetition, but I think the `config` macro is taking a `Lit`?
    #[config(env = "MIRRORD_FILE_PATH_INCLUDE")]
    pub include: Option<VecOrSingle<String>>,

    #[config(env = "MIRRORD_FILE_PATH_EXCLUDE")]
    pub exclude: Option<VecOrSingle<String>>,
}

impl MirrordToggleableConfig for FileFilterOption {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(FileFilterConfig {
            include: FromEnv::new(INCLUDE).source_value(),
            exclude: FromEnv::new(EXCLUDE).source_value(),
        })
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
        with_env_vars(vec![(INCLUDE, include.0), (EXCLUDE, exclude.0)], || {
            let env = FileFilterOption::default().generate_config().unwrap();

            assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
            assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
        });
    }

    #[rstest]
    fn disabled_config(
        #[values((None, None), (Some("IVAR1"), Some("IVAR1")))] include: (
            Option<&str>,
            Option<&str>,
        ),
        #[values((None, None), (Some("EVAR1"), Some("EVAR1")))] exclude: (
            Option<&str>,
            Option<&str>,
        ),
    ) {
        with_env_vars(vec![(INCLUDE, include.0), (EXCLUDE, exclude.0)], || {
            let env = ToggleableConfig::<FileFilterOption>::Enabled(false)
                .generate_config()
                .unwrap();

            assert_eq!(env.include.map(|vec| vec.join(";")).as_deref(), include.1);
            assert_eq!(env.exclude.map(|vec| vec.join(";")).as_deref(), exclude.1);
        });
    }
}

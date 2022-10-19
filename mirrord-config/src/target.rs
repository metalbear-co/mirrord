use serde::Deserialize;

use crate::config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig};

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged, rename_all = "lowercase")]
pub enum TargetFileConfig {
    Simple(Option<String>),
    Advanced {
        path: Option<String>,
        namespace: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct TargetConfig {
    pub path: Option<String>,
    pub namespace: Option<String>,
}

impl Default for TargetFileConfig {
    fn default() -> Self {
        TargetFileConfig::Simple(None)
    }
}

impl MirrordConfig for TargetFileConfig {
    type Generated = TargetConfig;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let config = match self {
            TargetFileConfig::Simple(path) => TargetConfig {
                path: (FromEnv::new("MIRRORD_IMPERSONATED_TARGET"), path).source_value(),
                namespace: FromEnv::new("MIRRORD_TARGET_NAMESPACE").source_value(),
            },
            TargetFileConfig::Advanced { path, namespace } => TargetConfig {
                path: (FromEnv::new("MIRRORD_IMPERSONATED_TARGET"), path).source_value(),
                namespace: (FromEnv::new("MIRRORD_TARGET_NAMESPACE"), namespace).source_value(),
            },
        };

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{config::MirrordConfig, util::testing::with_env_vars};

    #[rstest]
    fn default(
        #[values((None, None), (Some("foobar"), Some("foobar")))] path: (
            Option<&str>,
            Option<&str>,
        ),
        #[values((None, None))] namespace: (Option<&str>, Option<&str>),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_IMPERSONATED_TARGET", path.0),
                ("MIRRORD_TARGET_NAMESPACE", namespace.0),
            ],
            || {
                let target = TargetFileConfig::default().generate_config().unwrap();

                assert_eq!(target.path.as_deref(), path.1);
                assert_eq!(target.namespace.as_deref(), namespace.1);
            },
        );
    }
}

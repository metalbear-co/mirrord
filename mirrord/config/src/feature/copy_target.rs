use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
    util::MirrordToggleableConfig,
};

#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "CopyTargetFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct CopyTargetConfig {
    #[config(env = "MIRRORD_COPY_TARGET_ENABLED")]
    pub enabled: bool,

    #[config(env = "MIRRORD_COPY_TARGET_NAME")]
    pub name: Option<String>,
}

impl MirrordToggleableConfig for CopyTargetFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        let enabled = FromEnv::new("MIRRORD_COPY_TARGET_ENABLED")
            .source_value(context)
            .transpose()?
            .unwrap_or(false);

        let name = FromEnv::new("MIRRORD_COPY_TARGET_NAME")
            .source_value(context)
            .transpose()?;

        Ok(CopyTargetConfig { enabled, name })
    }
}

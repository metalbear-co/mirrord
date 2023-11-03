use mirrord_analytics::CollectAnalytics;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::{ConfigContext, ConfigError, FromMirrordConfig, MirrordConfig, Result},
    util::MirrordToggleableConfig,
};

/// TODO json schema
#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct CopyTargetFileConfig {
    pub scale_down: Option<bool>,
}

impl MirrordConfig for CopyTargetFileConfig {
    type Generated = CopyTargetConfig;

    fn generate_config(self, _context: &mut ConfigContext) -> Result<Self::Generated> {
        Ok(Self::Generated {
            enabled: true,
            scale_down: self.scale_down.unwrap_or_default(),
        })
    }
}

impl MirrordToggleableConfig for CopyTargetFileConfig {
    fn disabled_config(_context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        Ok(Self::Generated {
            enabled: false,
            scale_down: false,
        })
    }
}

impl FromMirrordConfig for CopyTargetConfig {
    type Generator = CopyTargetFileConfig;
}

#[derive(Clone, Debug)]
pub struct CopyTargetConfig {
    pub enabled: bool,
    pub scale_down: bool,
}

impl CollectAnalytics for &CopyTargetConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("enabled", self.enabled);
        analytics.add("scale_down", self.scale_down);
    }
}

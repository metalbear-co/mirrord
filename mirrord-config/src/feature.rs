use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::source::MirrordConfigSource, env::EnvFileConfig, fs::FsUserConfig,
    network::NetworkFileConfig, util::ToggleableConfig,
};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = FeatureConfig)]
pub struct FeatureFileConfig {
    #[serde(default)]
    #[config(nested)]
    pub env: ToggleableConfig<EnvFileConfig>,

    #[serde(default)]
    #[config(nested)]
    pub fs: ToggleableConfig<FsUserConfig>,

    #[serde(default)]
    #[config(nested)]
    pub network: ToggleableConfig<NetworkFileConfig>,

    #[serde(default)]
    #[config(env = "MIRRORD_CAPTURE_ERROR_TRACE", default = "false")]
    pub capture_error_trace: Option<bool>,
}

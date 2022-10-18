use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::source::MirrordConfigSource, env::EnvFileConfig, file::FileFilterOption, fs::FsConfig,
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
    pub fs: ToggleableConfig<FsConfig>,

    #[serde(default)]
    #[config(nested)]
    pub network: ToggleableConfig<NetworkFileConfig>,

    #[serde(default)]
    #[config(nested)]
    pub file_select: ToggleableConfig<FileFilterOption>,
}

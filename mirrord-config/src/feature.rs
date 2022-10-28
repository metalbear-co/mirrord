use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::source::MirrordConfigSource, env::EnvFileConfig, fs::FsUserConfig,
    network::NetworkFileConfig, util::ToggleableConfig,
};

/// Configuration for mirrord features.
#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = FeatureConfig)]
pub struct FeatureFileConfig {
    /// Controls the environment variables feature, see [`EnvFileConfig`].
    #[serde(default)]
    #[config(nested)]
    pub env: ToggleableConfig<EnvFileConfig>,

    /// Controls the file operations feature, see [`FsUserConfig`].
    #[serde(default)]
    #[config(nested)]
    pub fs: ToggleableConfig<FsUserConfig>,

    /// Controls the network feature, see [`NetworkFileConfig`].
    #[serde(default)]
    #[config(nested)]
    pub network: ToggleableConfig<NetworkFileConfig>,
}

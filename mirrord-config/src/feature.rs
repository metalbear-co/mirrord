use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::source::MirrordConfigSource, env::EnvFileConfig, fs::FsConfig,
    network::NetworkFileConfig, util::FlagedConfig,
};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[config(map_to = FeatureConfig)]
pub struct FeatureFileConfig {
    #[config(nested)]
    pub env: Option<FlagedConfig<EnvFileConfig>>,

    #[config(nested)]
    pub fs: Option<FlagedConfig<FsConfig>>,

    #[config(nested)]
    pub network: Option<FlagedConfig<NetworkFileConfig>>,
}

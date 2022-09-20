use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::config::{
    env::EnvField,
    fs::FsField,
    network::NetworkField,
    util::{ConfigError, FlagField, MirrordConfig},
};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FeatureField {
    #[nested]
    pub env: Option<FlagField<EnvField>>,

    #[nested]
    pub fs: Option<FlagField<FsField>>,

    #[nested]
    pub network: Option<FlagField<NetworkField>>,
}

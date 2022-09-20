use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::source::MirrordConfigSource, env::EnvField, fs::FsField, network::NetworkField,
    util::FlagField,
};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FeatureField {
    #[config(nested = true)]
    pub env: Option<FlagField<EnvField>>,

    #[config(nested = true)]
    pub fs: Option<FlagField<FsField>>,

    #[config(nested = true)]
    pub network: Option<FlagField<NetworkField>>,
}

use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{source::MirrordConfigSource, ConfigError},
    incoming::IncomingField,
    outgoing::OutgoingField,
    util::{FlagField, MirrordFlaggedConfig},
};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct NetworkField {
    #[config(nested = true)]
    pub incoming: Option<IncomingField>,

    #[config(nested = true)]
    pub outgoing: Option<FlagField<OutgoingField>>,

    #[config(unwrap = true, env = "MIRRORD_REMOTE_DNS", default = "true")]
    pub dns: Option<bool>,
}

impl MirrordFlaggedConfig for NetworkField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(MappedNetworkField {
            incoming: IncomingField::Mirror,
            dns: std::env::var("MIRRORD_REMOTE_DNS")
                .ok()
                .and_then(|val| val.parse().ok())
                .unwrap_or(false),
            outgoing: OutgoingField::disabled_config()?,
        })
    }
}

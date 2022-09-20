use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{source::MirrordConfigSource, ConfigError},
    util::MirrordFlaggedConfig,
};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[config(map_to = MappedOutgoingField)]
pub struct OutgoingField {
    #[config(unwrap = true, env = "MIRRORD_TCP_OUTGOING", default = "true")]
    pub tcp: Option<bool>,

    #[config(unwrap = true, env = "MIRRORD_UDP_OUTGOING", default = "true")]
    pub udp: Option<bool>,
}

impl MirrordFlaggedConfig for OutgoingField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(MappedOutgoingField {
            tcp: std::env::var("MIRRORD_TCP_OUTGOING")
                .ok()
                .and_then(|val| val.parse().ok())
                .unwrap_or(false),
            udp: std::env::var("MIRRORD_UDP_OUTGOING")
                .ok()
                .and_then(|val| val.parse().ok())
                .unwrap_or(false),
        })
    }
}

use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{
        default_value::DefaultValue, from_env::FromEnv, source::MirrordConfigSource, ConfigError,
    },
    util::MirrordFlaggedConfig,
};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[config(map_to = MappedOutgoingField)]
pub struct OutgoingField {
    #[config(env = "MIRRORD_TCP_OUTGOING", default = "true")]
    pub tcp: Option<bool>,

    #[config(env = "MIRRORD_UDP_OUTGOING", default = "true")]
    pub udp: Option<bool>,
}

impl MirrordFlaggedConfig for OutgoingField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(MappedOutgoingField {
            tcp: (
                FromEnv::new("MIRRORD_TCP_OUTGOING"),
                DefaultValue::new("false"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided("NetworkField", "incoming"))?,
            udp: (
                FromEnv::new("MIRRORD_UDP_OUTGOING"),
                DefaultValue::new("false"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided("NetworkField", "incoming"))?,
        })
    }
}

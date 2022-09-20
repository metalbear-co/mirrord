use mirrord_macro::MirrordConfig;
use serde::Deserialize;

use crate::util::{ConfigError, MirrordFlaggedConfig};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[mapto(MappedOutgoingField)]
pub struct OutgoingField {
    #[default_value("true")]
    #[from_env("MIRRORD_TCP_OUTGOING")]
    pub tcp: Option<bool>,

    #[default_value("true")]
    #[from_env("MIRRORD_UDP_OUTGOING")]
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

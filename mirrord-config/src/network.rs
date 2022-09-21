use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{
        default_value::DefaultValue, from_env::FromEnv, source::MirrordConfigSource, ConfigError,
    },
    incoming::IncomingField,
    outgoing::OutgoingField,
    util::{FlagField, MirrordFlaggedConfig},
};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct NetworkField {
    #[config(env = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC", default = "mirror")]
    pub incoming: Option<IncomingField>,

    #[config(nested = true)]
    pub outgoing: Option<FlagField<OutgoingField>>,

    #[config(env = "MIRRORD_REMOTE_DNS", default = "true")]
    pub dns: Option<bool>,
}

impl MirrordFlaggedConfig for NetworkField {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(MappedNetworkField {
            incoming: (
                FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC"),
                DefaultValue::new("mirror"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided("NetworkField", "incoming"))?,
            dns: (
                FromEnv::new("MIRRORD_REMOTE_DNS"),
                DefaultValue::new("false"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided("NetworkField", "dns"))?,
            outgoing: OutgoingField::disabled_config()?,
        })
    }
}

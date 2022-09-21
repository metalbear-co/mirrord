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

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{config::MirrordConfig, util::testing::with_env_vars};

    #[rstest]
    fn default(
        #[values((None, IncomingField::Mirror), (Some("false"), IncomingField::Mirror), (Some("true"), IncomingField::Steal))]
        incoming: (Option<&str>, IncomingField),
        #[values((None, true), (Some("false"), false))] dns: (Option<&str>, bool),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_AGENT_TCP_STEAL_TRAFFIC", incoming.0),
                ("MIRRORD_REMOTE_DNS", dns.0),
            ],
            || {
                let env = NetworkField::default().generate_config().unwrap();

                assert_eq!(env.incoming, incoming.1);
                assert_eq!(env.dns, dns.1);
            },
        );
    }
}

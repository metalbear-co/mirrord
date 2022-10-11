use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{
        default_value::DefaultValue, from_env::FromEnv, source::MirrordConfigSource, ConfigError,
    },
    incoming::IncomingConfig,
    outgoing::OutgoingFileConfig,
    util::{MirrordToggleableConfig, ToggleableConfig},
};

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = NetworkConfig)]
pub struct NetworkFileConfig {
    #[config(env = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC", default = "mirror")]
    pub incoming: Option<IncomingConfig>,

    #[serde(default)]
    #[config(nested)]
    pub outgoing: ToggleableConfig<OutgoingFileConfig>,

    #[config(env = "MIRRORD_REMOTE_DNS", default = "true")]
    pub dns: Option<bool>,
}

impl MirrordToggleableConfig for NetworkFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(NetworkConfig {
            incoming: (
                FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC"),
                DefaultValue::new("mirror"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided(
                    "NetworkFileConfig",
                    "incoming",
                    Some("MIRRORD_AGENT_TCP_STEAL_TRAFFIC"),
                ))?,
            dns: (
                FromEnv::new("MIRRORD_REMOTE_DNS"),
                DefaultValue::new("false"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided(
                    "NetworkFileConfig",
                    "dns",
                    Some("MIRRORD_REMOTE_DNS"),
                ))?,
            outgoing: OutgoingFileConfig::disabled_config()?,
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
        #[values((None, IncomingConfig::Mirror), (Some("false"), IncomingConfig::Mirror), (Some("true"), IncomingConfig::Steal))]
        incoming: (Option<&str>, IncomingConfig),
        #[values((None, true), (Some("false"), false))] dns: (Option<&str>, bool),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_AGENT_TCP_STEAL_TRAFFIC", incoming.0),
                ("MIRRORD_REMOTE_DNS", dns.0),
            ],
            || {
                let env = NetworkFileConfig::default().generate_config().unwrap();

                assert_eq!(env.incoming, incoming.1);
                assert_eq!(env.dns, dns.1);
            },
        );
    }
}

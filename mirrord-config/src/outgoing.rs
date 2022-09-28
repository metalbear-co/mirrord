use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    config::{
        default_value::DefaultValue, from_env::FromEnv, source::MirrordConfigSource, ConfigError,
    },
    util::MirrordToggleableConfig,
};

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[config(map_to = OutgoingConfig)]
pub struct OutgoingFileConfig {
    #[config(env = "MIRRORD_TCP_OUTGOING", default = "true")]
    pub tcp: Option<bool>,

    #[config(env = "MIRRORD_UDP_OUTGOING", default = "true")]
    pub udp: Option<bool>,
}

impl MirrordToggleableConfig for OutgoingFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(OutgoingConfig {
            tcp: (
                FromEnv::new("MIRRORD_TCP_OUTGOING"),
                DefaultValue::new("false"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided(
                    "OutgoingFileConfig",
                    "tcp",
                    Some("MIRRORD_TCP_OUTGOING"),
                ))?,
            udp: (
                FromEnv::new("MIRRORD_UDP_OUTGOING"),
                DefaultValue::new("false"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided(
                    "OutgoingFileConfig",
                    "udp",
                    Some("MIRRORD_TCP_OUTGOING"),
                ))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        util::{testing::with_env_vars, ToggleableConfig},
    };

    #[rstest]
    fn default(
        #[values((None, true), (Some("false"), false), (Some("true"), true))] tcp: (
            Option<&str>,
            bool,
        ),
        #[values((None, true), (Some("false"), false), (Some("true"), true))] udp: (
            Option<&str>,
            bool,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp.0),
                ("MIRRORD_UDP_OUTGOING", udp.0),
            ],
            || {
                let outgoing = OutgoingFileConfig::default().generate_config().unwrap();

                assert_eq!(outgoing.tcp, tcp.1);
                assert_eq!(outgoing.udp, udp.1);
            },
        );
    }

    #[rstest]
    fn disabled(
        #[values((None, false), (Some("false"), false), (Some("true"), true))] tcp: (
            Option<&str>,
            bool,
        ),
        #[values((None, false), (Some("false"), false), (Some("true"), true))] udp: (
            Option<&str>,
            bool,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp.0),
                ("MIRRORD_UDP_OUTGOING", udp.0),
            ],
            || {
                let outgoing = ToggleableConfig::<OutgoingFileConfig>::Enabled(false)
                    .generate_config()
                    .unwrap();

                assert_eq!(outgoing.tcp, tcp.1);
                assert_eq!(outgoing.udp, udp.1);
            },
        );
    }
}

use mirrord_config_derive::MirrordConfig;
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

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        util::{testing::with_env_vars, FlagField},
    };

    #[rstest]
    #[case(None, None, true, true)]
    #[case(None, Some("false"), true, false)]
    #[case(Some("false"), Some("false"), false, false)]
    #[case(Some("true"), Some("true"), true, true)]
    fn enabled(
        #[case] tcp_var: Option<&str>,
        #[case] udp_var: Option<&str>,
        #[case] tcp: bool,
        #[case] udp: bool,
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp_var),
                ("MIRRORD_UDP_OUTGOING", udp_var),
            ],
            || {
                let outgoing = OutgoingField::default().generate_config().unwrap();

                assert_eq!(outgoing.tcp, tcp);
                assert_eq!(outgoing.udp, udp);
            },
        );
    }

    #[rstest]
    #[case(None, None, false, false)]
    #[case(None, Some("false"), false, false)]
    #[case(Some("false"), Some("false"), false, false)]
    #[case(Some("true"), Some("true"), true, true)]
    fn disabled(
        #[case] tcp_var: Option<&str>,
        #[case] udp_var: Option<&str>,
        #[case] tcp: bool,
        #[case] udp: bool,
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp_var),
                ("MIRRORD_UDP_OUTGOING", udp_var),
            ],
            || {
                let outgoing = FlagField::<OutgoingField>::Enabled(false)
                    .generate_config()
                    .unwrap();

                assert_eq!(outgoing.tcp, tcp);
                assert_eq!(outgoing.udp, udp);
            },
        );
    }
}

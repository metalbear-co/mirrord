use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    outgoing::unix::{UnixTcpConfig, UnixTcpFileConfig},
    util::MirrordToggleableConfig,
};

pub mod unix;

#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "OutgoingFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct OutgoingConfig {
    #[config(env = "MIRRORD_TCP_OUTGOING", default = true)]
    pub tcp: bool,

    #[config(env = "MIRRORD_UDP_OUTGOING", default = true)]
    pub udp: bool,

    /// Consider removing when adding https://github.com/metalbear-co/mirrord/issues/702
    #[config(unstable, default = false)]
    pub ignore_localhost: bool,

    /// Control which paths for unix sockets (host-local interprocess communication) are connected
    /// remotely. When your application connects to a path that is configured to be connected
    /// remotely, mirrord will connect it to that path on the remote target.
    ///
    /// When disabled, all unix socket connections will be preformed locally on your system.
    ///
    /// See [`UnixTcpConfig`].
    #[config(nested, toggleable)]
    pub unix_tcp: UnixTcpConfig,
}

impl MirrordToggleableConfig for OutgoingFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(OutgoingConfig {
            tcp: FromEnv::new("MIRRORD_TCP_OUTGOING")
                .source_value()
                .unwrap_or(Ok(false))?,
            udp: FromEnv::new("MIRRORD_UDP_OUTGOING")
                .source_value()
                .unwrap_or(Ok(false))?,
            unix_tcp: UnixTcpFileConfig::disabled_config()?,
            ..Default::default()
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

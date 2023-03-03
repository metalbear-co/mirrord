use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    incoming::{IncomingConfig, IncomingFileConfig},
    outgoing::{OutgoingConfig, OutgoingFileConfig},
    util::MirrordToggleableConfig,
};

/// Controls mirrord network operations.
///
/// See the network traffic [reference](https://mirrord.dev/docs/reference/traffic/)
/// for more details.
///
/// ## Examples
///
/// - Steal incoming traffic, enable TCP outgoing traffic and DNS resolution:
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.network]
/// incoming = "steal"
/// dns = true # not needed, as this is the default
///
/// [feature.network.outgoing]
/// tcp = true
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "NetworkFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct NetworkConfig {
    /// Handles incoming network traffic, see [`IncomingConfig`] for more details.
    #[config(toggleable, nested)]
    pub incoming: IncomingConfig,

    /// Tunnel outgoing network operations through mirrord.
    #[config(toggleable, nested)]
    pub outgoing: OutgoingConfig,

    /// Resolve DNS via the remote pod.
    #[config(env = "MIRRORD_REMOTE_DNS", default = true)]
    pub dns: bool,
}

impl MirrordToggleableConfig for NetworkFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let dns = FromEnv::new("MIRRORD_REMOTE_DNS")
            .source_value()
            .transpose()?
            .unwrap_or(false);

        Ok(NetworkConfig {
            incoming: IncomingFileConfig::disabled_config()?,
            dns,
            outgoing: OutgoingFileConfig::disabled_config()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        incoming::{IncomingConfig, IncomingMode},
        util::testing::with_env_vars,
    };

    #[rstest]
    fn default(
        #[values(
            (
                None,
                IncomingConfig { mode: IncomingMode::Mirror, ..Default::default() }
            ),
            (
                Some("false"),
                IncomingConfig { mode: IncomingMode::Mirror, ..Default::default() }
            ),
            (
                Some("true"),
                IncomingConfig { mode: IncomingMode::Steal, ..Default::default() }
            ),
        )]
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

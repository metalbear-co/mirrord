#![feature(slice_concat_trait)]
#![feature(once_cell)]

pub mod agent;
pub mod config;
pub mod env;
pub mod feature;
pub mod fs;
pub mod incoming;
pub mod network;
pub mod outgoing;
pub mod pod;
pub mod util;

use std::path::Path;

use mirrord_config_derive::MirrordConfig;
use serde::Deserialize;

use crate::{
    agent::AgentFileConfig, config::source::MirrordConfigSource, feature::FeatureFileConfig,
    pod::PodFileConfig, util::VecOrSingle,
};

/// This is the root struct for mirrord-layer's configuration
#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[config(map_to = LayerConfig)]
pub struct LayerFileConfig {
    #[config(env = "MIRRORD_ACCEPT_INVALID_CERTIFICATES", default = "false")]
    pub accept_invalid_certificates: Option<bool>,

    #[config(env = "MIRRORD_SKIP_PROCESSES")]
    pub skip_processes: Option<VecOrSingle<String>>,

    #[serde(default)]
    #[config(nested)]
    pub agent: AgentFileConfig,

    #[serde(default)]
    #[config(nested)]
    pub pod: PodFileConfig,

    #[serde(default)]
    #[config(nested)]
    pub feature: FeatureFileConfig,
}

impl LayerFileConfig {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let file = std::fs::read(path)?;

        match path.extension().and_then(|os_val| os_val.to_str()) {
            Some("json") => serde_json::from_slice::<Self>(&file[..]).map_err(|err| err.into()),
            Some("toml") => toml::from_slice::<Self>(&file[..]).map_err(|err| err.into()),
            Some("yaml") => serde_yaml::from_slice::<Self>(&file[..]).map_err(|err| err.into()),
            _ => Err(anyhow::Error::msg("unsupported file format")),
        }
    }
}

#[cfg(test)]
mod tests {

    use rstest::*;

    use super::*;
    use crate::{
        fs::FsConfig, incoming::IncomingConfig, network::NetworkFileConfig,
        outgoing::OutgoingFileConfig, util::ToggleableConfig,
    };

    #[derive(Debug)]
    enum ConfigType {
        Json,
        Toml,
        Yaml,
    }

    impl ConfigType {
        fn empty(&self) -> &'static str {
            match self {
                ConfigType::Json => "{}",
                ConfigType::Toml => "",
                ConfigType::Yaml => "",
            }
        }

        fn full(&self) -> &'static str {
            match self {
                ConfigType::Json => {
                    r#"
                    {
                        "accept_invalid_certificates": false,
                        "agent": {
                            "log_level": "info",
                            "namespace": "default",
                            "image": "",
                            "image_pull_policy": "",
                            "ttl": 60,
                            "ephemeral": false
                        },
                        "feature": {
                            "env": true,
                            "fs": "write",
                            "network": {
                                "dns": false,
                                "incoming": "mirror",
                                "outgoing": {
                                    "tcp": true,
                                    "udp": false
                                }
                            }
                        },
                        "pod": {
                            "name": "test-service-abcdefg-abcd",
                            "namespace": "default",
                            "container": "test"
                        }
                    }
                    "#
                }
                ConfigType::Toml => {
                    r#"
                    accept_invalid_certificates = false

                    [agent]
                    log_level = "info"
                    namespace = "default"
                    image = ""
                    image_pull_policy = ""
                    ttl = 60
                    ephemeral = false

                    [feature]
                    env = true
                    fs = "write"

                    [feature.network]
                    dns = false
                    incoming = "mirror"

                    [feature.network.outgoing]
                    tcp = true
                    udp = false

                    [pod]
                    name = "test-service-abcdefg-abcd"
                    namespace = "default"
                    container = "test"
                    "#
                }
                ConfigType::Yaml => {
                    r#"
                    accept_invalid_certificates: false

                    agent:
                        log_level: "info"
                        namespace: "default"
                        image: ""
                        image_pull_policy: ""
                        ttl: 60
                        ephemeral: false

                    feature:
                        env: true
                        fs: "write"
                        network:
                            dns: false
                            incoming: "mirror"
                            outgoing:
                                tcp: true
                                udp: false
                    pod:
                        name: "test-service-abcdefg-abcd"
                        namespace: "default"
                        container: "test"
                    "#
                }
            }
        }

        fn parse(&self, value: &str) -> LayerFileConfig {
            match self {
                ConfigType::Json => {
                    serde_json::from_str(value).unwrap_or_else(|err| panic!("{:?}", err))
                }
                ConfigType::Toml => toml::from_str(value).unwrap_or_else(|err| panic!("{:?}", err)),
                ConfigType::Yaml => {
                    serde_yaml::from_str(value).unwrap_or_else(|err| panic!("{:?}", err))
                }
            }
        }
    }

    #[rstest]
    fn empty(
        #[values(ConfigType::Json, ConfigType::Toml, ConfigType::Yaml)] config_type: ConfigType,
    ) {
        let input = config_type.empty();

        let config = config_type.parse(input);

        assert_eq!(config, LayerFileConfig::default());
    }

    #[rstest]
    fn full(
        #[values(ConfigType::Json, ConfigType::Toml, ConfigType::Yaml)] config_type: ConfigType,
    ) {
        let input = config_type.full();

        let config = config_type.parse(input);

        let expect = LayerFileConfig {
            accept_invalid_certificates: Some(false),
            skip_processes: None,
            agent: AgentFileConfig {
                log_level: Some("info".to_owned()),
                namespace: Some("default".to_owned()),
                image: Some("".to_owned()),
                image_pull_policy: Some("".to_owned()),
                ttl: Some(60),
                ephemeral: Some(false),
                communication_timeout: None,
            },
            feature: FeatureFileConfig {
                env: ToggleableConfig::Enabled(true),
                fs: ToggleableConfig::Config(FsConfig::Write),
                network: ToggleableConfig::Config(NetworkFileConfig {
                    dns: Some(false),
                    incoming: Some(IncomingConfig::Mirror),
                    outgoing: ToggleableConfig::Config(OutgoingFileConfig {
                        tcp: Some(true),
                        udp: Some(false),
                    }),
                }),
            },
            pod: PodFileConfig {
                name: Some("test-service-abcdefg-abcd".to_owned()),
                namespace: Some("default".to_owned()),
                container: Some("test".to_owned()),
            },
        };

        assert_eq!(config, expect);
    }
}

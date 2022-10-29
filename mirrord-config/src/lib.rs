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

// TODO(alex) [mid] 2022-10-27: Add example usage of each field.

/// This is the root struct for mirrord-layer's configuration
#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[config(map_to = LayerConfig)]
pub struct LayerFileConfig {
    /// Controls whether or not mirrord accepts invalid TLS certificates (e.g. self-signed
    /// certificates).
    #[config(env = "MIRRORD_ACCEPT_INVALID_CERTIFICATES", default = "false")]
    pub accept_invalid_certificates: Option<bool>,

    /// Allows mirrord to skip unwanted processes.
    ///
    /// Useful when process A spawns process B, and the user wants mirrord to operate only on
    /// process B.
    #[config(env = "MIRRORD_SKIP_PROCESSES")]
    pub skip_processes: Option<VecOrSingle<String>>,

    /// The target that will be impersonated by mirrord.
    ///
    /// Supports `pod`, `podname`, `deployment`, `container`, `containername`.
    #[config(env = "MIRRORD_IMPERSONATED_TARGET")]
    pub target: Option<String>,

    /// Namespace where the `target` lives.
    #[config(env = "MIRRORD_TARGET_NAMESPACE")]
    pub target_namespace: Option<String>,

    /// IP:PORT to connect to instead of using k8s api, for testing purposes.
    #[cfg_attr(feature = "schema", schemars(skip))]
    #[config(env = "MIRRORD_CONNECT_TCP")]
    pub connect_tcp: Option<String>,

    /// Agent name that already exists that we can connect to.
    #[cfg_attr(feature = "schema", schemars(skip))]
    #[config(env = "MIRRORD_CONNECT_AGENT")]
    pub connect_agent_name: Option<String>,

    /// Agent listen port that already exists that we can connect to.
    #[cfg_attr(feature = "schema", schemars(skip))]
    #[config(env = "MIRRORD_CONNECT_PORT")]
    pub connect_agent_port: Option<u16>,

    /// Agent configuration, see [`agent::AgentFileConfig`].
    #[serde(default)]
    #[config(nested)]
    pub agent: AgentFileConfig,

    // START | To be removed after deprecated functionality is removed
    #[serde(default)]
    #[config(nested)]
    pub pod: PodFileConfig,
    // END
    /// Controls mirrord features, see [`feature::FeatureFileConfig`].
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
        fs::{FsModeConfig, FsUserConfig},
        incoming::IncomingConfig,
        network::NetworkFileConfig,
        outgoing::OutgoingFileConfig,
        util::ToggleableConfig,
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
                        "target": "pod/test-service-abcdefg-abcd",
                        "target_namespace": "default",
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
                    target = "pod/test-service-abcdefg-abcd"
                    target_namespace = "default"

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
                    target: "pod/test-service-abcdefg-abcd"
                    target_namespace: "default"

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
            connect_agent_name: None,
            connect_agent_port: None,
            target: Some("pod/test-service-abcdefg-abcd".to_owned()),
            target_namespace: Some("default".to_owned()),
            skip_processes: None,
            agent: AgentFileConfig {
                log_level: Some("info".to_owned()),
                namespace: Some("default".to_owned()),
                image: Some("".to_owned()),
                image_pull_policy: Some("".to_owned()),
                ttl: Some(60),
                ephemeral: Some(false),
                communication_timeout: None,
                startup_timeout: None,
            },
            feature: FeatureFileConfig {
                env: ToggleableConfig::Enabled(true),
                fs: ToggleableConfig::Config(FsUserConfig::Simple(FsModeConfig::Write)),
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
            connect_tcp: None,
        };

        assert_eq!(config, expect);
    }

    /// Helper for printing the config schema.
    ///
    /// Run it with:
    ///
    /// ```sh
    /// cargo test -p mirrord-config print_schema --features schema -- --ignored --nocapture
    /// ```
    #[cfg(feature = "schema")]
    #[test]
    #[ignore]
    fn print_schema() {
        let schema = schemars::schema_for!(LayerFileConfig);
        println!("{}", serde_json::to_string_pretty(&schema).unwrap());
    }
}

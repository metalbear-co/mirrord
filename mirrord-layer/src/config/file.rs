use std::{fs, path::Path};

use mirrord_macro::MirrordConfig;
use serde::Deserialize;
use thiserror::Error;

use crate::config::{
    env::LayerEnvConfig,
    util::{FlagField, VecOrSingle},
    LayerConfig,
};

pub trait MirrordConfig {
    type Generated;

    fn generate_config(self) -> Result<Self::Generated, ConfigError>;
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("couldn't find value for {0:?}")]
    ValueNotProvided(String),
}

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct AgentField {
    #[default_value("info")]
    #[from_env("MIRRORD_AGENT_RUST_LOG")]
    log_level: Option<String>,

    #[from_env("MIRRORD_AGENT_NAMESPACE")]
    namespace: Option<String>,

    #[from_env("MIRRORD_AGENT_IMAGE")]
    image: Option<String>,

    #[default_value("IfNotPresent")]
    #[from_env("MIRRORD_AGENT_IMAGE_PULL_POLICY")]
    image_pull_policy: Option<String>,

    #[default_value("0")]
    #[from_env("MIRRORD_AGENT_TTL")]
    ttl: Option<u16>,

    #[default_value("false")]
    #[from_env("MIRRORD_EPHEMERAL_CONTAINER")]
    ephemeral: Option<bool>,

    #[from_env("MIRRORD_AGENT_COMMUNICATION_TIMEOUT")]
    communication_timeout: Option<u16>,
}

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PodField {
    #[unwrap_option]
    #[from_env("MIRRORD_AGENT_IMPERSONATED_POD_NAME")]
    name: Option<String>,

    #[default_value("default")]
    #[from_env("MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE")]
    namespace: Option<String>,

    #[from_env("MIRRORD_IMPERSONATED_CONTAINER_NAME")]
    container: Option<String>,
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct EnvField {
    include: Option<VecOrSingle<String>>,
    exclude: Option<VecOrSingle<String>>,
}

#[derive(Debug)]
pub struct MappedEnvField {
    include: Option<String>,
    exclude: Option<String>,
}

impl MirrordConfig for Option<FlagField<EnvField>> {
    type Generated = MappedEnvField;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        let exclude = std::env::var("MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE")
            .ok()
            .or_else(|| {
                self.as_ref().and_then(|flag| {
                    flag.enabled_map(
                        |enabled| if enabled { None } else { Some("".to_owned()) },
                        |env| env.exclude.clone().map(|exclude| exclude.join(";")),
                    )
                })
            });

        let include = std::env::var("MIRRORD_OVERRIDE_ENV_VARS_INCLUDE")
            .ok()
            .or_else(|| {
                self.as_ref().and_then(|flag| {
                    flag.enabled_map(
                        |enabled| if enabled { None } else { Some("".to_owned()) },
                        |env| env.include.clone().map(|include| include.join(";")),
                    )
                })
            });

        Ok(MappedEnvField { include, exclude })
    }
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
enum FsField {
    Read,
    Write,
}

#[derive(MirrordConfig, Default, Deserialize, PartialEq, Eq, Clone, Debug)]
#[mapto(MappedOutgoingField)]
pub struct OutgoingField {
    #[default_value("true")]
    #[from_env("MIRRORD_TCP_OUTGOING")]
    tcp: Option<bool>,

    #[default_value("true")]
    #[from_env("MIRRORD_TCP_OUTGOING")]
    udp: Option<bool>,
}

impl MirrordConfig for Option<FlagField<OutgoingField>> {
    type Generated = MappedOutgoingField;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        match self {
            Some(FlagField::Enabled(true)) | None => OutgoingField::default().generate_config(),
            Some(FlagField::Enabled(false)) => Ok(MappedOutgoingField {
                tcp: false,
                udp: false,
            }),
            Some(FlagField::Config(config)) => config.generate_config(),
        }
    }
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct NetworkField {
    incoming: Option<ModeField>,
    outgoing: Option<FlagField<OutgoingField>>,
    dns: Option<bool>,
}

#[derive(Debug)]
pub struct MappedNetworkField {
    // pub outgoing: MappedOutgoingField,
}

impl MirrordConfig for Option<FlagField<NetworkField>> {
    type Generated = MappedNetworkField;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        // Ok(MappedNetworkField {
        //     outgoing: self.outgoing.generate_config()?,
        // })

        Ok(MappedNetworkField {})
    }
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
enum ModeField {
    Mirror,
    Steal,
}

#[derive(Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FeatureField {
    env: Option<FlagField<EnvField>>,

    fs: Option<FlagField<FsField>>,

    network: Option<FlagField<NetworkField>>,
}

#[derive(Debug)]
pub struct MappedFeatureField {
    pub env: MappedEnvField,
    pub network: MappedNetworkField,
}

impl MirrordConfig for FeatureField {
    type Generated = MappedFeatureField;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        Ok(MappedFeatureField {
            env: self.env.generate_config()?,
            network: self.network.generate_config()?,
        })
    }
}

#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug)]
#[serde(deny_unknown_fields)]
// #[mapto(LayerConfig)]
pub struct LayerFileConfig {
    #[default_value("false")]
    #[from_env("MIRRORD_ACCEPT_INVALID_CERTIFICATES")]
    accept_invalid_certificates: Option<bool>,

    #[serde(default)]
    agent: AgentField,

    #[serde(default)]
    pod: PodField,

    #[serde(default)]
    feature: FeatureField,
}

impl LayerFileConfig {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        let file = fs::read(path)?;

        match path.extension().and_then(|os_val| os_val.to_str()) {
            Some("json") => serde_json::from_slice::<Self>(&file[..]).map_err(|err| err.into()),
            Some("toml") => toml::from_slice::<Self>(&file[..]).map_err(|err| err.into()),
            Some("yaml") => serde_yaml::from_slice::<Self>(&file[..]).map_err(|err| err.into()),
            _ => Err(anyhow::Error::msg("unsupported file format")),
        }
    }

    pub fn merge_with(self, config: LayerEnvConfig) -> LayerConfig {
        let accept_invalid_certificates = config
            .accept_invalid_certificates
            .or(self.accept_invalid_certificates)
            .unwrap_or(false);

        // Agent

        let agent_rust_log = config
            .agent_rust_log
            .or(self.agent.log_level)
            .unwrap_or_else(|| "info".to_owned());

        let agent_namespace = config.agent_namespace.or(self.agent.namespace);

        let agent_image = config.agent_image.or(self.agent.image);

        let image_pull_policy = config
            .image_pull_policy
            .or(self.agent.image_pull_policy)
            .unwrap_or_else(|| "IfNotPresent".to_owned());

        let agent_ttl = config.agent_ttl.or(self.agent.ttl).unwrap_or(0);

        let agent_communication_timeout = config
            .agent_communication_timeout
            .or(self.agent.communication_timeout);

        let ephemeral_container = config
            .ephemeral_container
            .or(self.agent.ephemeral)
            .unwrap_or(false);

        // Pod

        let impersonated_pod_name = config
            .impersonated_pod_name
            .or(self.pod.name)
            .expect("Must set MIRRORD_AGENT_IMPERSONATED_POD_NAME or pod.name value in config");

        let impersonated_pod_namespace = config
            .impersonated_pod_namespace
            .or(self.pod.namespace)
            .unwrap_or_else(|| "default".to_owned());

        let impersonated_container_name = config.impersonated_container_name.or(self.pod.container);

        // Feature fs

        let enabled_file_ops = config
            .enabled_file_ops
            .or_else(|| {
                self.feature
                    .fs
                    .as_ref()
                    .map(|flag| flag.enabled_map(|_| false, |fs| fs == &FsField::Write))
            })
            .unwrap_or(false);

        let enabled_file_ro_ops = config
            .enabled_file_ro_ops
            .or_else(|| {
                self.feature
                    .fs
                    .as_ref()
                    .map(|flag| flag.enabled_or_equal(&FsField::Read))
            })
            .unwrap_or(true);

        // Feature env

        let override_env_vars_exclude = config.override_env_vars_exclude.or_else(|| {
            self.feature.env.as_ref().and_then(|flag| {
                flag.enabled_map(
                    |enabled| if enabled { None } else { Some("".to_owned()) },
                    |env| env.exclude.clone().map(|exclude| exclude.join(";")),
                )
            })
        });

        let override_env_vars_include = config.override_env_vars_include.or_else(|| {
            self.feature.env.as_ref().and_then(|flag| {
                flag.enabled_map(
                    |enabled| if enabled { None } else { Some("".to_owned()) },
                    |env| env.include.clone().map(|include| include.join(";")),
                )
            })
        });

        // Feature network

        let agent_tcp_steal_traffic = config
            .agent_tcp_steal_traffic
            .or_else(|| {
                self.feature.network.as_ref().map(|network| {
                    network.map(|network| network.incoming == Some(ModeField::Steal))
                })
            })
            .unwrap_or(false);

        let remote_dns = config
            .remote_dns
            .or_else(|| {
                self.feature
                    .network
                    .as_ref()
                    .and_then(|network| network.map(|network| network.dns))
            })
            .unwrap_or(true);

        let enabled_tcp_outgoing = config
            .enabled_tcp_outgoing
            .or_else(|| {
                self.feature.network.as_ref().and_then(|network| {
                    network.map(|network| {
                        network
                            .outgoing
                            .as_ref()
                            .and_then(|outgoing| outgoing.map(|outgoing| outgoing.tcp))
                    })
                })
            })
            .unwrap_or(true);

        let enabled_udp_outgoing = config
            .enabled_udp_outgoing
            .or_else(|| {
                self.feature.network.as_ref().and_then(|network| {
                    network.map(|network| {
                        network
                            .outgoing
                            .as_ref()
                            .and_then(|outgoing| outgoing.map(|outgoing| outgoing.udp))
                    })
                })
            })
            .unwrap_or(true);

        LayerConfig {
            agent_rust_log,
            agent_namespace,
            agent_image,
            image_pull_policy,
            impersonated_pod_name,
            impersonated_pod_namespace,
            impersonated_container_name,
            accept_invalid_certificates,
            agent_ttl,
            agent_tcp_steal_traffic,
            agent_communication_timeout,
            enabled_file_ops,
            enabled_file_ro_ops,
            override_env_vars_exclude,
            override_env_vars_include,
            ephemeral_container,
            remote_dns,
            enabled_tcp_outgoing,
            enabled_udp_outgoing,
        }
    }
}

#[cfg(test)]
mod tests {

    use rstest::*;

    use super::*;

    #[derive(Debug)]
    enum ConfigType {
        Json,
        Toml,
        Yaml,
    }

    impl ConfigType {
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
    fn generate_config(#[values(ConfigType::Json)] config_type: ConfigType) {
        println!(
            "{:#?}",
            config_type
                .parse(
                    r#"{ 
                        "pod": { 
                            "name": "test"
                        },
                        "feature": {
                            "env": {
                                "include": ["a", "b"],
                                "exclude": ["C", "D"]
                            }
                        }
                    }"#
                )
                .generate_config()
        );
    }

    #[rstest]
    fn full(
        #[values(ConfigType::Json, ConfigType::Toml, ConfigType::Yaml)] config_type: ConfigType,
    ) {
        let input = config_type.full();

        let config = config_type.parse(input);

        let expect = LayerFileConfig {
            accept_invalid_certificates: Some(false),
            agent: AgentField {
                log_level: Some("info".to_owned()),
                namespace: Some("default".to_owned()),
                image: Some("".to_owned()),
                image_pull_policy: Some("".to_owned()),
                ttl: Some(60),
                ephemeral: Some(false),
                communication_timeout: None,
            },
            feature: FeatureField {
                env: Some(FlagField::Enabled(true)),
                fs: Some(FlagField::Config(FsField::Write)),
                network: Some(FlagField::Config(NetworkField {
                    dns: Some(false),
                    incoming: Some(ModeField::Mirror),
                    outgoing: Some(FlagField::Config(OutgoingField {
                        tcp: Some(true),
                        udp: Some(false),
                    })),
                })),
            },
            pod: PodField {
                name: Some("test-service-abcdefg-abcd".to_owned()),
                namespace: Some("default".to_owned()),
                container: Some("test".to_owned()),
            },
        };

        assert_eq!(config, expect);
    }

    #[test]
    fn merge() {
        let file_config = LayerFileConfig {
            accept_invalid_certificates: Some(true),
            agent: AgentField {
                log_level: Some("agent_rust_log".to_owned()),
                namespace: Some("agent_namespace".to_owned()),
                image: Some("agent_image".to_owned()),
                image_pull_policy: Some("image_pull_policy".to_owned()),
                ttl: Some(32),
                ephemeral: Some(true),
                communication_timeout: Some(31),
            },
            feature: FeatureField {
                env: Some(FlagField::Config(EnvField {
                    include: Some(VecOrSingle::Single("include".to_owned())),
                    exclude: Some(VecOrSingle::Multiple(vec![
                        "exclude".to_owned(),
                        "exclude2".to_owned(),
                    ])),
                })),
                fs: Some(FlagField::Enabled(true)),
                network: Some(FlagField::Config(NetworkField {
                    incoming: Some(ModeField::Steal),
                    dns: Some(false),
                    outgoing: Some(FlagField::Config(OutgoingField {
                        tcp: Some(false),
                        udp: Some(false),
                    })),
                })),
            },
            pod: PodField {
                name: Some("impersonated_pod_name".to_owned()),
                namespace: Some("impersonated_pod_namespace".to_owned()),
                container: Some("impersonated_container_name".to_owned()),
            },
        };

        let config = file_config.merge_with(Default::default());

        assert_eq!(config.agent_rust_log, "agent_rust_log");
        assert_eq!(config.agent_namespace, Some("agent_namespace".to_owned()));
        assert_eq!(config.agent_image, Some("agent_image".to_owned()));
        assert_eq!(config.image_pull_policy, "image_pull_policy");
        assert_eq!(config.impersonated_pod_name, "impersonated_pod_name");
        assert_eq!(
            config.impersonated_pod_namespace,
            "impersonated_pod_namespace"
        );
        assert_eq!(
            config.impersonated_container_name,
            Some("impersonated_container_name".to_owned())
        );
        assert_eq!(config.accept_invalid_certificates, true);
        assert_eq!(config.agent_ttl, 32);
        assert_eq!(config.agent_tcp_steal_traffic, true);
        assert_eq!(config.agent_communication_timeout, Some(31));
        assert_eq!(config.enabled_file_ops, false);
        assert_eq!(config.enabled_file_ro_ops, true);
        assert_eq!(
            config.override_env_vars_exclude,
            Some("exclude;exclude2".to_owned())
        );
        assert_eq!(config.override_env_vars_include, Some("include".to_owned()));
        assert_eq!(config.ephemeral_container, true);
        assert_eq!(config.remote_dns, false);
        assert_eq!(config.enabled_tcp_outgoing, false);
        assert_eq!(config.enabled_udp_outgoing, false);
    }

    #[rstest]
    fn merge_fs(
        #[values(
            (None, (false, true)),
            (Some(FlagField::Enabled(true)), (false, true)),
            (Some(FlagField::Enabled(false)), (false, false)),
            (Some(FlagField::Config(FsField::Read)), (false, true)),
            (Some(FlagField::Config(FsField::Write)), (true, false))
        )]
        param: (Option<FlagField<FsField>>, (bool, bool)),
    ) {
        let (fs, expect) = param;

        let file_config = LayerFileConfig {
            feature: FeatureField {
                fs,
                ..Default::default()
            },
            ..Default::default()
        };

        let config = file_config.merge_with(LayerEnvConfig {
            impersonated_pod_name: Some("".to_owned()),
            ..Default::default()
        });

        assert_eq!(
            (config.enabled_file_ops, config.enabled_file_ro_ops),
            expect
        );
    }
}

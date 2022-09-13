use std::{fs, path::Path};

use serde::Deserialize;

use crate::config::{env::LayerEnvConfig, LayerConfig};

#[derive(Deserialize, Default, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct AgentField {
    log_level: Option<String>,
    namespace: Option<String>,
    image: Option<String>,
    image_pull_policy: Option<String>,
    ttl: Option<u16>,
    ephemeral: Option<bool>,
    communication_timeout: Option<u16>,
}

#[derive(Deserialize, Default, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct PodField {
    name: Option<String>,
    namespace: Option<String>,
    container: Option<String>,
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct EnvField {
    include: Option<String>,
    exclude: Option<String>,
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(untagged)]
enum FlagField<T> {
    Enabled(bool),
    Config(T),
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "snake_case")]
enum IOField {
    Read,
    Write,
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct NetworkField {
    tcp: Option<FlagField<IOField>>,
    udp: Option<FlagField<IOField>>,
    dns: Option<bool>,
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
enum ModeField {
    Mirror,
    Steal,
}

#[derive(Deserialize, Default, PartialEq, Clone, Debug)]
struct FeatureField {
    env: Option<FlagField<EnvField>>,
    fs: Option<FlagField<IOField>>,
    network: Option<FlagField<NetworkField>>,
}

#[derive(Deserialize, Default, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct LayerFileConfig {
    accept_invalid_certificates: Option<bool>,
    agent: AgentField,
    mode: Option<ModeField>,
    pod: PodField,
    #[serde(default)]
    feature: FeatureField,
}

impl LayerFileConfig {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        match path.extension().and_then(|os_val| os_val.to_str()) {
            Some("json") => {
                let file = fs::read(path)?;
                serde_json::from_slice::<Self>(&file[..]).map_err(|err| err.into())
            }
            Some("toml") => {
                let file = fs::read(path)?;
                toml::from_slice::<Self>(&file[..]).map_err(|err| err.into())
            }
            Some("yaml") => {
                let file = fs::read(path)?;
                serde_yaml::from_slice::<Self>(&file[..]).map_err(|err| err.into())
            }
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

        // Mode

        let agent_tcp_steal_traffic = config
            .agent_tcp_steal_traffic
            .unwrap_or_else(|| self.mode == Some(ModeField::Steal));

        // Feature fs

        let enabled_file_ops = config
            .enabled_file_ops
            .or(self.feature.fs.clone().map(|flag| match flag {
                FlagField::Enabled(val) => val,
                FlagField::Config(io) => io == IOField::Write,
            }))
            .unwrap_or(false);

        let enabled_file_ro_ops = config
            .enabled_file_ro_ops
            .or(self.feature.fs.clone().map(|flag| match flag {
                FlagField::Enabled(_) => false,
                FlagField::Config(io) => io == IOField::Read,
            }))
            .unwrap_or(true);

        // Feature env

        let override_env_vars_exclude =
            config
                .override_env_vars_exclude
                .or(self.feature.env.clone().and_then(|flag| match flag {
                    FlagField::Enabled(true) => None,
                    FlagField::Enabled(false) => Some("".to_owned()),
                    FlagField::Config(env) => env.exclude,
                }));

        let override_env_vars_include =
            config
                .override_env_vars_include
                .or(self.feature.env.clone().and_then(|flag| match flag {
                    FlagField::Enabled(true) => None,
                    FlagField::Enabled(false) => Some("".to_owned()),
                    FlagField::Config(env) => env.include,
                }));

        // Feature network

        let remote_dns = config
            .remote_dns
            .or(self
                .feature
                .network
                .clone()
                .and_then(|network| match network {
                    FlagField::Enabled(val) => Some(val),
                    FlagField::Config(network) => network.dns,
                }))
            .unwrap_or(true);

        let enabled_tcp_outgoing = config
            .enabled_tcp_outgoing
            .or(self.feature.network.clone().map(|network| match network {
                FlagField::Enabled(val) => val,
                FlagField::Config(network) => {
                    network.tcp == Some(FlagField::Config(IOField::Write))
                        || network.tcp == Some(FlagField::Enabled(true))
                }
            }))
            .unwrap_or(true);

        let enabled_udp_outgoing = config
            .enabled_udp_outgoing
            .or(self.feature.network.map(|network| match network {
                FlagField::Enabled(val) => val,
                FlagField::Config(network) => {
                    network.udp == Some(FlagField::Config(IOField::Write))
                        || network.udp == Some(FlagField::Enabled(true))
                }
            }))
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
                        "mode": "mirror",
                        "feature": {
                            "env": true,
                            "fs": "write",
                            "network": {
                                "tcp": "read",
                                "udp": false,
                                "dns": false
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
                    mode = "mirror"

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
                    tcp = "read"
                    udp = false
                    dns = false

                    [pod]
                    name = "test-service-abcdefg-abcd"
                    namespace = "default"
                    container = "test"
                    "#
                }
                ConfigType::Yaml => {
                    r#"
                    accept_invalid_certificates: false
                    mode: "mirror"

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
                            tcp: "read"
                            udp: false
                            dns: false
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
                fs: Some(FlagField::Config(IOField::Write)),
                network: Some(FlagField::Config(NetworkField {
                    tcp: Some(FlagField::Config(IOField::Read)),
                    udp: Some(FlagField::Enabled(false)),
                    dns: Some(false),
                })),
            },
            mode: Some(ModeField::Mirror),
            pod: PodField {
                name: Some("test-service-abcdefg-abcd".to_owned()),
                namespace: Some("default".to_owned()),
                container: Some("test".to_owned()),
            },
        };

        assert_eq!(config, expect);
    }
}

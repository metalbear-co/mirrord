use std::{fs, path::Path, slice::Join};

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
#[serde(untagged)]
enum VecOrSingle<T> {
    Single(T),
    Multiple(Vec<T>),
}

impl<T> VecOrSingle<T> {
    fn join<Separator>(self, sep: Separator) -> <[T] as Join<Separator>>::Output
    where
        [T]: Join<Separator>,
    {
        match self {
            VecOrSingle::Single(val) => [val].join(sep),
            VecOrSingle::Multiple(vals) => vals.join(sep),
        }
    }
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct EnvField {
    include: Option<VecOrSingle<String>>,
    exclude: Option<VecOrSingle<String>>,
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(untagged)]
enum FlagField<T> {
    Enabled(bool),
    Config(T),
}

impl<T> FlagField<T> {
    fn enabled_or_equal<'a, Rhs>(&'a self, comp: Rhs) -> bool
    where
        &'a T: PartialEq<Rhs>,
    {
        match self {
            FlagField::Enabled(enabled) => *enabled,
            FlagField::Config(val) => val == comp,
        }
    }

    fn map<'a, R, C>(&'a self, config_cb: C) -> R
    where
        R: From<bool>,
        C: FnOnce(&'a T) -> R,
    {
        match self {
            FlagField::Enabled(enabled) => R::from(*enabled),
            FlagField::Config(val) => config_cb(val),
        }
    }

    fn map_or_enabled<'a, R, E, C>(&'a self, enabled_cb: E, config_cb: C) -> R
    where
        E: FnOnce(bool) -> R,
        C: FnOnce(&'a T) -> R,
    {
        match self {
            FlagField::Enabled(enabled) => enabled_cb(*enabled),
            FlagField::Config(val) => config_cb(val),
        }
    }
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
enum IOField {
    Read,
    Write,
}
#[derive(Deserialize, PartialEq, Clone, Debug)]
struct OutgoingField {
    tcp: Option<bool>,
    udp: Option<bool>,
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct NetworkField {
    incoming: Option<ModeField>,
    outgoing: Option<FlagField<OutgoingField>>,
    dns: Option<bool>,
}

#[derive(Deserialize, PartialEq, Clone, Debug)]
#[serde(rename_all = "lowercase")]
enum ModeField {
    Mirror,
    Steal,
}

#[derive(Deserialize, Default, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
struct FeatureField {
    env: Option<FlagField<EnvField>>,
    fs: Option<FlagField<IOField>>,
    network: Option<NetworkField>,
}

#[derive(Deserialize, Default, PartialEq, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct LayerFileConfig {
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

        // Feature fs

        let enabled_file_ops = config
            .enabled_file_ops
            .or_else(|| {
                self.feature
                    .fs
                    .as_ref()
                    .map(|flag| flag.enabled_or_equal(&IOField::Write))
            })
            .unwrap_or(false);

        let enabled_file_ro_ops = config
            .enabled_file_ro_ops
            .or_else(|| {
                self.feature
                    .fs
                    .as_ref()
                    .map(|flag| flag.enabled_or_equal(&IOField::Read))
            })
            .unwrap_or(true);

        // Feature env

        let override_env_vars_exclude = config.override_env_vars_exclude.or_else(|| {
            self.feature.env.as_ref().and_then(|flag| {
                flag.map_or_enabled(
                    |enabled| if enabled { None } else { Some("".to_owned()) },
                    |env| env.exclude.clone().map(|exclude| exclude.join(";")),
                )
            })
        });

        let override_env_vars_include = config.override_env_vars_include.or_else(|| {
            self.feature.env.as_ref().and_then(|flag| {
                flag.map_or_enabled(
                    |enabled| if enabled { None } else { Some("".to_owned()) },
                    |env| env.include.clone().map(|include| include.join(";")),
                )
            })
        });

        // Feature network

        let agent_tcp_steal_traffic = config
            .agent_tcp_steal_traffic
            .or_else(|| {
                self.feature
                    .network
                    .as_ref()
                    .map(|network| network.incoming == Some(ModeField::Steal))
            })
            .unwrap_or(false);

        let remote_dns = config
            .remote_dns
            .or_else(|| {
                self.feature
                    .network
                    .as_ref()
                    .and_then(|network| network.dns)
            })
            .unwrap_or(true);

        let enabled_tcp_outgoing = config
            .enabled_tcp_outgoing
            .or_else(|| {
                self.feature.network.as_ref().and_then(|network| {
                    network
                        .outgoing
                        .as_ref()
                        .and_then(|outgoing| outgoing.map(|outgoing| outgoing.tcp))
                })
            })
            .unwrap_or(true);

        let enabled_udp_outgoing = config
            .enabled_udp_outgoing
            .or_else(|| {
                self.feature.network.as_ref().and_then(|network| {
                    network
                        .outgoing
                        .as_ref()
                        .and_then(|outgoing| outgoing.map(|outgoing| outgoing.udp))
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
                network: Some(NetworkField {
                    dns: Some(false),
                    incoming: Some(ModeField::Mirror),
                    outgoing: Some(FlagField::Config(OutgoingField {
                        tcp: Some(true),
                        udp: Some(false),
                    })),
                }),
            },
            pod: PodField {
                name: Some("test-service-abcdefg-abcd".to_owned()),
                namespace: Some("default".to_owned()),
                container: Some("test".to_owned()),
            },
        };

        assert_eq!(config, expect);
    }
}

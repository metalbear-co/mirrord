use std::{fs, path::Path};

use serde::Deserialize;

use crate::config::LayerConfig;

#[derive(Deserialize, Default, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct AgentField {
    log_level: Option<String>,
    namespace: Option<String>,
    image: Option<String>,
    image_pull_policy: Option<String>,
    ttl: Option<u16>,
    ephemeral: Option<bool>,
}

#[derive(Deserialize, Default, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct PodField {
    name: String,
    namespace: Option<String>,
    container: Option<String>,
}

#[derive(Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct EnvField {
    include: Option<String>,
    exclude: Option<String>,
}

#[derive(Deserialize, PartialEq, Debug)]
#[serde(untagged)]
enum FlagField<T> {
    Enabled(bool),
    Config(T),
}

#[derive(Deserialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
enum IOField {
    Read,
    Write,
}

#[derive(Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct NetworkField {
    tcp: Option<FlagField<IOField>>,
    udp: Option<FlagField<IOField>>,
    dns: Option<bool>,
}

#[derive(Deserialize, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
enum ModeField {
    Mirror,
    Steal,
}

#[derive(Deserialize, Default, PartialEq, Debug)]
struct FeatureField {
    env: Option<FlagField<EnvField>>,
    fs: Option<FlagField<IOField>>,
    network: Option<FlagField<NetworkField>>,
}

#[derive(Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct ExecArgFile {
    accept_invalid_certificates: Option<bool>,
    agent: AgentField,
    mode: Option<ModeField>,
    pod: PodField,
    #[serde(default)]
    feature: FeatureField,
}

impl ExecArgFile {
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

    pub fn merge_with(&self, _config: LayerConfig) -> LayerConfig {
        todo!();
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

        fn parse(&self, value: &str) -> ExecArgFile {
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

        let expect = ExecArgFile {
            accept_invalid_certificates: Some(false),
            agent: AgentField {
                log_level: Some("info".to_owned()),
                namespace: Some("default".to_owned()),
                image: Some("".to_owned()),
                image_pull_policy: Some("".to_owned()),
                ttl: Some(60),
                ephemeral: Some(false),
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
                name: "test-service-abcdefg-abcd".to_owned(),
                namespace: Some("default".to_owned()),
                container: Some("test".to_owned()),
            },
        };

        assert_eq!(config, expect);
    }
}

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
pub mod target;
pub mod util;

/// To generate the `mirrord-schema.json` file see
/// [`tests::check_schema_file_exists_and_is_valid_or_create_it`].
///
/// Remember to re-generate the `mirrord-schema.json` if you make **ANY** changes to this lib,
/// including if you only made documentation changes.
use std::path::Path;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    agent::AgentFileConfig, config::source::MirrordConfigSource, feature::FeatureFileConfig,
    target::TargetFileConfig, util::VecOrSingle,
};

/// Main struct for mirrord-layer's configuration
///
/// ## Examples
///
/// - Run mirrord with read-only file operations, mirroring traffic, skipping unwanted processes:
///
/// ```toml
/// # mirrord-config.toml
///
/// target = "pod/sample-pod-1234"
/// skip_processes = ["ide-debugger", "ide-service"] # we don't want mirrord to hook into these
///
/// [agent]
/// log_level = "debug"
/// ttl = 1024 # seconds
///
/// [feature]
/// fs = "read" # default
///
/// [feature.network]
/// incoming = "mirror" # default
/// ```
///
/// - Run mirrord with read-write file operations, stealing traffic, accept local TLS certificates,
///   use a custom mirrord-agent image:
///
/// ```toml
/// # mirrord-config.toml
///
/// target = "pod/sample-pod-1234"
/// accept_invalid_certificates = true
///
/// [agent]
/// log_level = "debug"
/// ttl = 1024 # seconds
/// image = "registry/mirrord-agent-custom:latest"
/// image_pull_policy = "Always"
///
/// [feature]
/// fs = "write"
///
/// [feature.network]
/// incoming = "steal"
/// ```
#[derive(MirrordConfig, Deserialize, Default, PartialEq, Eq, Clone, Debug, JsonSchema)]
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

    /// Specifies the running pod to mirror.
    ///
    /// Supports:
    /// - `pod/{sample-pod}/[container]/{sample-container}`;
    /// - `podname/{sample-pod}/[container]/{sample-container}`;
    /// - `deployment/{sample-deployment}/[container]/{sample-container}`;
    #[serde(default)]
    #[config(nested)]
    pub target: TargetFileConfig,

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

    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
    };

    use rstest::*;
    use schemars::schema::RootSchema;

    use super::*;
    use crate::{
        fs::{FsModeConfig, FsUserConfig},
        incoming::IncomingConfig,
        network::NetworkFileConfig,
        outgoing::OutgoingFileConfig,
        target::{PodTarget, Target, TargetFileConfig},
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
                        "target": {
                            "path": "pod/test-service-abcdefg-abcd",
                            "namespace": "default"
                        },
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
                        }
                    }
                    "#
                }
                ConfigType::Toml => {
                    r#"
                    accept_invalid_certificates = false

                    [target]
                    path = "pod/test-service-abcdefg-abcd"
                    namespace = "default"

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
                    "#
                }
                ConfigType::Yaml => {
                    r#"
                    accept_invalid_certificates: false
                    target:
                        path: "pod/test-service-abcdefg-abcd"
                        namespace: "default"

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
            target: TargetFileConfig::Advanced {
                path: Some(Target::Pod(PodTarget {
                    pod: "test-service-abcdefg-abcd".to_owned(),
                    container: None,
                })),
                namespace: Some("default".to_owned()),
            },
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
                capture_error_trace: None,
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
    /// cargo test -p mirrord-config print_schema -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn print_schema() {
        let schema = schemars::schema_for!(LayerFileConfig);
        println!("{}", serde_json::to_string_pretty(&schema).unwrap());
    }

    const SCHEMA_FILE_PATH: &str = "./../mirrord-schema.json";

    /// Writes the config schema to a file (uploaded to the schema store).
    fn write_schema_to_file(schema: &RootSchema) -> File {
        println!("Writing schema to file.");

        let content = serde_json::to_string_pretty(&schema).expect("Failed generating schema!");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .read(true)
            .open(SCHEMA_FILE_PATH)
            .expect("Failed to create schema file!");

        file.write(content.as_bytes())
            .expect("Failed writing schema to file!");

        file
    }

    /// Checks if a schema file already exists, otherwise generates the schema and creates the file.
    ///
    /// It also checks and updates when the schema file is outdated.
    ///
    /// Use this function to generate a mirrord config schema file.
    ///
    /// ```sh
    /// cargo test -p mirrord-config check_schema_file_exists_and_is_valid_or_create_it -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn check_schema_file_exists_and_is_valid_or_create_it() {
        let fresh_schema = schemars::schema_for!(LayerFileConfig);
        let fresh_content =
            serde_json::to_string_pretty(&fresh_schema).expect("Failed generating schema!");

        println!("Checking for an existing schema file!");
        let mut existing_content = String::with_capacity(fresh_content.len());
        if let Ok(_) = File::open(SCHEMA_FILE_PATH)
            .and_then(|mut file| file.read_to_string(&mut existing_content))
        {
            if existing_content != fresh_content {
                println!("Schema is outdated, preparing updated version!");
                write_schema_to_file(&fresh_schema);
            }
        } else {
            write_schema_to_file(&fresh_schema);
        }
    }

    #[test]
    fn test_schema_file_exists() {
        let _ = File::open(SCHEMA_FILE_PATH).expect("Schema file doesn't exist!");
    }

    #[test]
    fn test_schema_file_is_up_to_date() {
        let compare_schema = schemars::schema_for!(LayerFileConfig);
        let compare_content =
            serde_json::to_string_pretty(&compare_schema).expect("Failed generating schema!");

        let mut existing_content = String::with_capacity(compare_content.len());
        let _ = File::open(SCHEMA_FILE_PATH)
            .unwrap()
            .read_to_string(&mut existing_content);

        assert_eq!(existing_content, compare_content);
    }
}

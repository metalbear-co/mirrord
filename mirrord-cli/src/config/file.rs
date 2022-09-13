#![allow(dead_code)]

use std::{fs, path::Path};

use serde::Deserialize;

use crate::config::ExecArgs;

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
#[derive(PartialEq)]
struct AgentField {
    log_level: Option<String>,
    namespace: Option<String>,
    image: Option<String>,
    image_pull_policy: Option<String>,
    ttl: Option<u16>,
    ephemeral: Option<bool>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
#[derive(PartialEq)]
struct PodField {
    name: String,
    namespace: Option<String>,
    container: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
#[derive(PartialEq)]
struct EnvField {
    include: Option<String>,
    exclude: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
#[cfg(test)]
#[derive(PartialEq)]
enum FlagField<T> {
    Enabled(bool),
    Config(T),
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
#[cfg(test)]
#[derive(PartialEq)]
enum IOField {
    Read,
    Write,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
#[derive(PartialEq)]
struct NetworkField {
    tcp: Option<FlagField<IOField>>,
    udp: Option<FlagField<IOField>>,
    dns: Option<bool>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
#[derive(PartialEq)]
struct TargetField {
    binary: String,
    args: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
#[cfg(test)]
#[derive(PartialEq)]
enum ModeField {
    Mirror,
    Steal,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ExecArgFile {
    accept_invalid_certificates: Option<bool>,
    agent: Option<AgentField>,
    env: Option<FlagField<EnvField>>,
    fs: Option<FlagField<IOField>>,
    mode: Option<ModeField>,
    network: Option<NetworkField>,
    pod: Option<PodField>,
    target: Option<TargetField>,
}

impl ExecArgFile {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        match path.extension().and_then(|os_val| os_val.to_str()) {
            Some("json") => {
                let file = fs::read(path)?;
                serde_json::from_slice::<Self>(&file[..]).map_err(|err| err.into())
            }
            _ => Err(anyhow::Error::msg("unsupported file format")),
        }
    }

    pub fn merge_with(&self, _args: ExecArgs) -> ExecArgs {
        todo!();
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn json() {
        let input = r#"
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
            "env": true,
            "fs": "write",
            "mode": "mirror",
            "network": {
              "tcp": "read",
              "udp": false,
              "dns": false
            },
            "pod": {
              "name": "test-service-abcdefg-abcd",
              "namespace": "default",
              "container": "test"
            },
            "target": {
              "binary": "node",
              "args": "server.js"
            }
          }
        "#;

        let config = serde_json::from_str::<ExecArgFile>(input).unwrap();

        let expect = ExecArgFile {
            accept_invalid_certificates: Some(false),
            agent: Some(AgentField {
                log_level: Some("info".to_owned()),
                namespace: Some("default".to_owned()),
                image: Some("".to_owned()),
                image_pull_policy: Some("".to_owned()),
                ttl: Some(60),
                ephemeral: Some(false),
            }),
            env: Some(FlagField::Enabled(true)),
            fs: Some(FlagField::Config(IOField::Write)),
            mode: Some(ModeField::Mirror),
            network: Some(NetworkField {
                tcp: Some(FlagField::Config(IOField::Read)),
                udp: Some(FlagField::Enabled(false)),
                dns: Some(false),
            }),
            pod: Some(PodField {
                name: "test-service-abcdefg-abcd".to_owned(),
                namespace: Some("default".to_owned()),
                container: Some("test".to_owned()),
            }),
            target: Some(TargetField {
                binary: "node".to_owned(),
                args: Some("server.js".to_owned()),
            }),
        };

        assert_eq!(
            config.accept_invalid_certificates,
            expect.accept_invalid_certificates
        );
        assert_eq!(config.agent, expect.agent);
        assert_eq!(config.env, expect.env);
        assert_eq!(config.fs, expect.fs);
        assert_eq!(config.mode, expect.mode);
        assert_eq!(config.network, expect.network);
        assert_eq!(config.pod, expect.pod);
        assert_eq!(config.target, expect.target);
    }
}

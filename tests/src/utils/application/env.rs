#![allow(dead_code)]

use crate::utils::application::GoVersion;

#[derive(Debug)]
pub enum EnvApp {
    Go(GoVersion),
    Bash,
    BashInclude,
    BashExclude,
    NodeInclude,
    NodeExclude,
}

impl EnvApp {
    pub fn command(&self) -> Vec<String> {
        match self {
            Self::Go(go_version) => vec![format!("go-e2e-env/{go_version}.go_test_app")],
            Self::Bash => ["bash", "bash-e2e/env.sh"].map(String::from).to_vec(),
            Self::BashInclude => ["bash", "bash-e2e/env.sh", "include"]
                .map(String::from)
                .to_vec(),
            Self::BashExclude => ["bash", "bash-e2e/env.sh", "exclude"]
                .map(String::from)
                .to_vec(),
            Self::NodeInclude => [
                "node",
                "node-e2e/remote_env/test_remote_env_vars_include_works.mjs",
            ]
            .map(String::from)
            .to_vec(),
            Self::NodeExclude => [
                "node",
                "node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs",
            ]
            .map(String::from)
            .to_vec(),
        }
    }

    pub fn mirrord_args(&self) -> Option<Vec<&str>> {
        match self {
            Self::BashInclude | Self::NodeInclude => Some(vec!["-s", "MIRRORD_FAKE_VAR_FIRST"]),
            Self::BashExclude | Self::NodeExclude => Some(vec!["-x", "MIRRORD_FAKE_VAR_FIRST"]),
            Self::Go(..) | Self::Bash => None,
        }
    }
}

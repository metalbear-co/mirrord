#![allow(dead_code)]

use crate::utils::application::{app_path, GoVersion};

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
            Self::Go(go_version) => vec![app_path(&format!("go-e2e-env/{go_version}.go_test_app"))],
            Self::Bash => vec!["bash".into(), app_path("bash-e2e/env.sh")],
            Self::BashInclude => {
                vec!["bash".into(), app_path("bash-e2e/env.sh"), "include".into()]
            }
            Self::BashExclude => {
                vec!["bash".into(), app_path("bash-e2e/env.sh"), "exclude".into()]
            }
            Self::NodeInclude => vec![
                "node".into(),
                app_path("node-e2e/remote_env/test_remote_env_vars_include_works.mjs"),
            ],
            Self::NodeExclude => vec![
                "node".into(),
                app_path("node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs"),
            ],
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

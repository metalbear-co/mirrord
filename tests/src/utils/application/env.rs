#![allow(dead_code)]

use mirrord_test_utils::node;

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
            Self::Go(go_version) => vec![format!("apps/go-e2e-env/go-e2e-env.{go_version}")],
            Self::Bash => vec!["bash".into(), "apps/bash-e2e/env/main.sh".to_owned()],
            Self::BashInclude => {
                vec![
                    "bash".into(),
                    "apps/bash-e2e/env/main.sh".to_owned(),
                    "include".into(),
                ]
            }
            Self::BashExclude => {
                vec![
                    "bash".into(),
                    "apps/bash-e2e/env/main.sh".to_owned(),
                    "exclude".into(),
                ]
            }
            Self::NodeInclude => vec![
                node(),
                "apps/node-e2e/remote-env/test-remote-env-vars-include-works/main.mjs".to_owned(),
            ],
            Self::NodeExclude => vec![
                node(),
                "apps/node-e2e/remote-env/test-remote-env-vars-exclude-works/main.mjs".to_owned(),
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

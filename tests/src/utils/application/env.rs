#[derive(Debug)]
pub enum EnvApp {
    Go21,
    Go22,
    Go23,
    Bash,
    BashInclude,
    BashExclude,
    NodeInclude,
    NodeExclude,
}

impl EnvApp {
    pub fn command(&self) -> Vec<&str> {
        match self {
            Self::Go21 => vec!["go-e2e-env/21.go_test_app"],
            Self::Go22 => vec!["go-e2e-env/22.go_test_app"],
            Self::Go23 => vec!["go-e2e-env/23.go_test_app"],
            Self::Bash => vec!["bash", "bash-e2e/env.sh"],
            Self::BashInclude => vec!["bash", "bash-e2e/env.sh", "include"],
            Self::BashExclude => vec!["bash", "bash-e2e/env.sh", "exclude"],
            Self::NodeInclude => vec![
                "node",
                "node-e2e/remote_env/test_remote_env_vars_include_works.mjs",
            ],
            Self::NodeExclude => vec![
                "node",
                "node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs",
            ],
        }
    }

    pub fn mirrord_args(&self) -> Option<Vec<&str>> {
        match self {
            Self::BashInclude | Self::NodeInclude => Some(vec!["-s", "MIRRORD_FAKE_VAR_FIRST"]),
            Self::BashExclude | Self::NodeExclude => Some(vec!["-x", "MIRRORD_FAKE_VAR_FIRST"]),
            Self::Go21 | Self::Go22 | Self::Go23 | Self::Bash => None,
        }
    }
}

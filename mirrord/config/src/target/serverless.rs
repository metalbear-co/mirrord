use std::str::Split;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FAIL_PARSE_DEPLOYMENT_OR_POD, FromSplit};
use crate::config::{ConfigError, Result};

/// - `serverless/{service-name}[/container/{hostname}]`;
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerlessTarget {
    pub serverless: String,
    // container holds the hostname of the container
    pub container: Option<String>,
}

impl ServerlessTarget {
    /// Session-manager rooms identify the service, while the replica selector narrows down to a
    /// specific container when needed.
    pub fn sessions_manager_room_id(&self) -> Result<String> {
        Ok(self.serverless.clone())
    }

    pub fn sessions_manager_target_replica_id(&self) -> Option<String> {
        self.container.clone()
    }
}

impl FromSplit for ServerlessTarget {
    fn from_split(split: &mut Split<char>) -> Result<Self> {
        let service = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned()))?;

        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                serverless: service.to_owned(),
                container: Some(container.to_owned()),
            }),
            (None, None) => Ok(Self {
                serverless: service.to_owned(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned(),
            )),
        }
    }
}

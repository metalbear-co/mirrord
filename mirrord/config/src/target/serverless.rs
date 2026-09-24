use std::str::Split;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FAIL_PARSE_DEPLOYMENT_OR_POD, FromSplit};
use crate::config::{ConfigError, Result};

/// Targets a serverless remote workload, i.e. an application running outside Kubernetes with a
/// `mirrord-agent` companion in each replica, reached through sessions-manager.
///
/// - `serverless/{service-name}[/container/{replica-id}]`;
///
/// The environment part of the sessions-manager service scope is not part of the target; it is
/// taken from `target.namespace`, defaulting to `default`.
///
/// The `container` field:
///
/// Restricts the session to the agent on this replica, as its `ReplicaId` (the agent's
/// `MIRRORD_REMOTE_SERVICE_REPLICA`, which defaults to the hosting platform's replica id such as
/// the ECS task ARN, and then to the container hostname). When unset, sessions-manager offers the
/// eligible agent with the lowest replica id.
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerlessTarget {
    /// Name of the service in sessions-manager: the logical workload whose replicas
    /// each run an agent companion. Together with the environment it forms the
    /// `ServiceScope` that the session's agent pairing is restricted to.
    pub serverless: String,
    pub container: Option<String>,
}

impl ServerlessTarget {
    pub fn sessions_manager_service(&self) -> Result<String> {
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

use std::time::Duration;

use chrono::{DateTime, Utc};
use kube::CustomResource;
use mirrord_config::target::Target;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordSession",
    status = "MirrordSessionStatus"
)]
pub struct MirrordSessionSpec {
    /// Session's target
    pub target: Target,

    pub max_time: Option<Duration>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct MirrordSessionStatus {
    /// Owner of this session
    pub owner: MirrordSessionOwner,

    /// Start time of this session when actual websocket connection is first created.
    pub start_time: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct MirrordSessionOwner {
    /// Unique ID.
    pub user_id: String,
    /// Creator local username.
    pub username: String,
    /// Creator hostname.
    pub hostname: String,
    /// Creator Kubernetes username.
    pub k8s_username: String,
}

use std::time::Duration;

use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordSession"
)]
pub struct MirrordSessionSpec {
    /// Owner of this session
    pub owner: MirrordSessionOwner,

    /// Start time of this session when actual websocket connection is first created.
    pub start_time: DateTime<Utc>,

    /// Max duration for this session
    pub max_time: Option<Duration>,
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

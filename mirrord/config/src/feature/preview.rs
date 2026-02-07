use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Controls the lifetime and creation behavior of preview sessions.
///
/// ```json
/// {
///   "feature": {
///     "preview": {
///       "image": "my-registry/my-app:latest",
///       "ttl_mins": 60,
///       "creation_timeout_secs": 60
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
#[config(map_to = "PreviewFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct PreviewConfig {
    /// #### feature.preview.image {#feature-preview-image}
    ///
    /// Container image to run in the preview pod.
    /// The image must be pre-built and pushed to a registry accessible by the cluster.
    #[config(env = "MIRRORD_PREVIEW_IMAGE")]
    pub image: Option<String>,

    /// #### feature.preview.ttl_mins {#feature-preview-ttl_mins}
    ///
    /// How long (in minutes) the preview session is allowed to live after creation.
    /// The operator will terminate the session when this time elapses.
    #[config(env = "MIRRORD_PREVIEW_TTL_MINS", default = 60)]
    pub ttl_mins: u64,

    /// #### feature.preview.creation_timeout_secs {#feature-preview-creation_timeout_secs}
    ///
    /// How long (in seconds) the CLI waits for the preview session to become ready.
    /// If the session hasn't reached `Ready` within this time, the CLI deletes it.
    #[config(env = "MIRRORD_PREVIEW_CREATION_TIMEOUT_SECS", default = 60)]
    pub creation_timeout_secs: u64,
}

impl CollectAnalytics for &PreviewConfig {
    fn collect_analytics(&self, _analytics: &mut mirrord_analytics::Analytics) {}
}

use std::{
    fmt::{self, Display},
    str::FromStr,
};

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    ///
    /// Set to `"infinite"` to disable TTL.
    #[config(env = "MIRRORD_PREVIEW_TTL_MINS", default = PreviewTtlMins::default())]
    pub ttl_mins: PreviewTtlMins,

    /// #### feature.preview.creation_timeout_secs {#feature-preview-creation_timeout_secs}
    ///
    /// How long (in seconds) the CLI waits for the preview session to become ready.
    /// If the session hasn't reached `Ready` within this time, the CLI deletes it.
    #[config(env = "MIRRORD_PREVIEW_CREATION_TIMEOUT_SECS", default = 60)]
    pub creation_timeout_secs: u64,
}

impl CollectAnalytics for &PreviewConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        let (ttl_mins, ttl_infinite) = match self.ttl_mins {
            PreviewTtlMins::Finite(ttl_mins) => (ttl_mins, false),
            PreviewTtlMins::Infinite(_) => (0, true),
        };

        analytics.add("image", self.image.is_some());
        analytics.add("ttl_mins", ttl_mins);
        analytics.add("ttl_infinite", ttl_infinite);
        analytics.add("creation_timeout_secs", self.creation_timeout_secs);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PreviewTtlMins {
    Finite(u64),
    Infinite(PreviewTtlKeyword),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PreviewTtlKeyword {
    Infinite,
}

impl Display for PreviewTtlMins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finite(mins) => write!(f, "{mins}"),
            Self::Infinite(_) => f.write_str("infinite"),
        }
    }
}

impl PreviewTtlMins {
    /// Sentinel value used in CRD `ttl_secs` to represent an infinite TTL.
    ///
    /// Any value greater than or equal to this sentinel is treated as infinite.
    pub const INFINITE_TTL_SECS: u64 = u32::MAX as u64;
}

impl Default for PreviewTtlMins {
    fn default() -> Self {
        Self::Finite(60)
    }
}

impl FromStr for PreviewTtlMins {
    type Err = PreviewTtlParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "infinite" {
            return Ok(Self::Infinite(PreviewTtlKeyword::Infinite));
        }

        value
            .parse()
            .map(Self::Finite)
            .map_err(|_| PreviewTtlParseError)
    }
}

#[derive(Debug, Error)]
#[error("preview ttl must be an integer or \"infinite\"")]
pub struct PreviewTtlParseError;

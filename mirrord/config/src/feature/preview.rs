use std::{
    fmt::{self, Display},
    str::FromStr,
};

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{ConfigError, source::MirrordConfigSource};

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
    ///
    /// Mutually exclusive with [`ttl_secs`](#feature-preview-ttl_secs); when neither is set,
    /// defaults to 60 minutes.
    #[config(env = "MIRRORD_PREVIEW_TTL_MINS")]
    pub ttl_mins: Option<PreviewTtl>,

    /// #### feature.preview.ttl_secs {#feature-preview-ttl_secs}
    ///
    /// Same as [`ttl_mins`](#feature-preview-ttl_mins) but expressed in seconds.
    ///
    /// Set to `"infinite"` to disable TTL.
    ///
    /// Mutually exclusive with [`ttl_mins`](#feature-preview-ttl_mins).
    #[config(env = "MIRRORD_PREVIEW_TTL_SECS")]
    pub ttl_secs: Option<PreviewTtl>,

    /// #### feature.preview.creation_timeout_secs {#feature-preview-creation_timeout_secs}
    ///
    /// How long (in seconds) the CLI waits for the preview session to become ready.
    /// If the session hasn't reached `Ready` within this time, the CLI deletes it.
    #[config(env = "MIRRORD_PREVIEW_CREATION_TIMEOUT_SECS", default = 60)]
    pub creation_timeout_secs: u64,
}

impl PreviewConfig {
    /// Default TTL in seconds when neither `ttl_mins` nor `ttl_secs` is set.
    pub const DEFAULT_TTL_SECS: u64 = 60 * 60;

    /// Returns the configured TTL converted to seconds, applying the default if neither
    /// `ttl_mins` nor `ttl_secs` is set. An infinite TTL (from either field) collapses to
    /// [`PreviewTtl::INFINITE_TTL_SECS`].
    pub fn resolved_ttl_secs(&self) -> u64 {
        match (self.ttl_secs, self.ttl_mins) {
            (Some(PreviewTtl::Infinite(_)), _) | (_, Some(PreviewTtl::Infinite(_))) => {
                PreviewTtl::INFINITE_TTL_SECS
            }
            (Some(PreviewTtl::Finite(secs)), _) => secs,
            (_, Some(PreviewTtl::Finite(mins))) => mins.saturating_mul(60),
            (None, None) => Self::DEFAULT_TTL_SECS,
        }
    }

    pub fn verify(&self) -> Result<(), ConfigError> {
        if self.ttl_mins.is_some() && self.ttl_secs.is_some() {
            return Err(ConfigError::Conflict(
                "`feature.preview.ttl_mins` and `feature.preview.ttl_secs` cannot both be set."
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

impl CollectAnalytics for &PreviewConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        let ttl_secs = self.resolved_ttl_secs();
        let ttl_infinite = ttl_secs >= PreviewTtl::INFINITE_TTL_SECS;

        analytics.add("image", self.image.is_some());
        analytics.add("ttl_secs", ttl_secs);
        analytics.add("ttl_infinite", ttl_infinite);
        analytics.add("creation_timeout_secs", self.creation_timeout_secs);
    }
}

/// A TTL value usable for either [`PreviewConfig::ttl_mins`] or [`PreviewConfig::ttl_secs`].
///
/// The unit (minutes or seconds) depends on which field carries the value; the variants
/// themselves are unit-agnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PreviewTtl {
    Finite(u64),
    Infinite(PreviewTtlKeyword),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PreviewTtlKeyword {
    Infinite,
}

impl Display for PreviewTtl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finite(value) => write!(f, "{value}"),
            Self::Infinite(_) => f.write_str("infinite"),
        }
    }
}

impl PreviewTtl {
    /// Sentinel value used in CRD `ttl_secs` to represent an infinite TTL.
    ///
    /// Any value greater than or equal to this sentinel is treated as infinite.
    pub const INFINITE_TTL_SECS: u64 = u32::MAX as u64;
}

impl FromStr for PreviewTtl {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigError;

    fn config_with_ttl(
        ttl_mins: Option<PreviewTtl>,
        ttl_secs: Option<PreviewTtl>,
    ) -> PreviewConfig {
        PreviewConfig {
            image: None,
            ttl_mins,
            ttl_secs,
            creation_timeout_secs: 60,
        }
    }

    #[test]
    fn resolved_ttl_defaults_when_neither_set() {
        let cfg = config_with_ttl(None, None);
        assert_eq!(cfg.resolved_ttl_secs(), PreviewConfig::DEFAULT_TTL_SECS);
    }

    #[test]
    fn resolved_ttl_uses_minutes_when_only_minutes_set() {
        let cfg = config_with_ttl(Some(PreviewTtl::Finite(5)), None);
        assert_eq!(cfg.resolved_ttl_secs(), 300);
    }

    #[test]
    fn resolved_ttl_uses_seconds_when_only_seconds_set() {
        let cfg = config_with_ttl(None, Some(PreviewTtl::Finite(123)));
        assert_eq!(cfg.resolved_ttl_secs(), 123);
    }

    #[test]
    fn resolved_ttl_recognises_infinite_from_either_field() {
        let cfg = config_with_ttl(
            Some(PreviewTtl::Infinite(PreviewTtlKeyword::Infinite)),
            None,
        );
        assert_eq!(cfg.resolved_ttl_secs(), PreviewTtl::INFINITE_TTL_SECS);
    }

    #[test]
    fn verify_rejects_both_ttl_fields() {
        let cfg = config_with_ttl(Some(PreviewTtl::Finite(5)), Some(PreviewTtl::Finite(60)));
        assert!(matches!(cfg.verify(), Err(ConfigError::Conflict(_))));
    }
}

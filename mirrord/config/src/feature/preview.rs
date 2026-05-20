use std::{
    fmt::{self, Display},
    path::PathBuf,
    str::FromStr,
};

use base64::prelude::*;
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

    /// #### feature.preview.config_mounts {#feature-preview-config_mounts}
    ///
    /// Files to mount into the preview pod at session start.
    ///
    /// Each entry projects a single file at an absolute path inside the
    /// container's filesystem, without shadowing the surrounding directory's
    /// other contents. Useful for overriding individual config files
    /// (`/etc/app/config.yaml`, `/etc/nginx/conf.d/custom.conf`, ...) without
    /// rebuilding the image.
    ///
    /// Files are read once at session creation and never refreshed — re-run
    /// `mirrord preview start` with the same `key` to change them.
    ///
    /// `config_mounts` is not a confidentiality boundary: the `payload` is
    /// stored on the `PreviewSession` resource and is visible to anyone with
    /// `get` permission on it. Do not put credentials or secrets here.
    ///
    /// Each entry sources its content one of two ways:
    /// - **Inline:** set `type` (`"text"` or `"binary"`) and `payload` directly.
    /// - **From file:** set `from_file` to a path on the local filesystem. The file is read at
    ///   session creation time; valid UTF-8 contents are sent as `"text"`, anything else is
    ///   base64-encoded and sent as `"binary"`. The local path is not transmitted to the operator.
    ///
    /// `payload`/`type` and `from_file` are mutually exclusive on a single entry.
    ///
    /// ```json
    /// {
    ///   "feature": {
    ///     "preview": {
    ///       "config_mounts": [
    ///         {
    ///           "mount_at": "/etc/app/config.yaml",
    ///           "type": "text",
    ///           "payload": "server:\n  listen: 8080\n"
    ///         },
    ///         {
    ///           "mount_at": "/usr/local/bin/probe",
    ///           "from_file": "./build/probe"
    ///         }
    ///       ]
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// ##### Implementation
    ///
    /// Each session creates one `ConfigMap` owned by the `PreviewSession`,
    /// holding all mount payloads. The ConfigMap is mounted into the preview pod
    /// with one per-file `subPath` bind per entry, so each mount overlays a
    /// single path without shadowing the surrounding directory. The ConfigMap is
    /// garbage-collected automatically when the session ends.
    ///
    /// Because both the `PreviewSession` resource and the generated `ConfigMap`
    /// live in etcd, the **combined size of all `payload` payloads in a single
    /// session is bound by Kubernetes' ~1 MiB per-object limit**. For `"text"`
    /// payloads that's roughly 1 MiB of content; for `"binary"`, base64
    /// inflates the encoded form by ~33%, leaving roughly 750 KiB of raw bytes.
    /// Exceeding the limit causes the apiserver to reject the session at
    /// creation time.
    #[config(default)]
    pub config_mounts: Vec<ConfigMount>,
}

/// A single file to project into the preview pod's container filesystem.
///
/// Either `payload`+`type` (inline) or `from_file` (load from disk) must
/// be set; the two forms are mutually exclusive. After
/// [`ConfigMount::resolve`] has been called, the result is guaranteed
/// to have `payload` and `r#type` set and `from_file` cleared. This form
/// gets sent to the operator.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ConfigMount {
    /// Absolute path inside the preview pod's container where the file should
    /// appear. The surrounding directory's other contents
    /// (if any) are left untouched, only this one path is overlaid.
    pub mount_at: String,

    /// How the `payload` payload should be interpreted when writing it to the
    /// file. `"text"` for verbatim UTF-8 content, `"binary"` for
    /// base64-encoded bytes. Required when `payload` is set; determined
    /// automatically when `from_file` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<ConfigMountType>,

    /// Inline file contents. Interpretation depends on `type`: for `"text"`,
    /// written to the file byte-for-byte; for `"binary"`, base64-decoded
    /// first. Mutually exclusive with `from_file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,

    /// Local filesystem path to read the file from. The contents are loaded
    /// at session creation time and sent as `"text"` (verbatim) if valid
    /// UTF-8, otherwise base64-encoded and sent as `"binary"`. Mutually
    /// exclusive with `payload`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_file: Option<PathBuf>,
}

impl ConfigMount {
    /// Returns a fully-resolved copy of this mount: `payload` and `r#type` are
    /// guaranteed `Some`, and `from_file` is cleared.
    ///
    /// If `from_file` is set, reads the file from disk; valid UTF-8 is
    /// returned as [`ConfigMountType::Text`], anything else is base64-encoded
    /// and returned as [`ConfigMountType::Binary`]. Otherwise the inline
    /// `payload`/`type` pair is passed through unchanged.
    ///
    /// Assumes [`PreviewConfig::verify`] has already accepted the mount.
    pub fn resolve(self) -> Result<Self, ConfigError> {
        let Some(file_path) = self.from_file else {
            return Ok(self);
        };

        let bytes = std::fs::read(&file_path).map_err(|error| ConfigError::FileAccessFailed {
            path: file_path.clone(),
            error,
        })?;

        let (kind, payload) = match String::from_utf8(bytes) {
            Ok(text) => (ConfigMountType::Text, text),
            Err(err) => (
                ConfigMountType::Binary,
                BASE64_STANDARD.encode(err.as_bytes()),
            ),
        };

        Ok(Self {
            mount_at: self.mount_at,
            r#type: Some(kind),
            payload: Some(payload),
            from_file: None,
        })
    }
}

/// Encoding of a [`ConfigMount::payload`] payload.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConfigMountType {
    /// `payload` is written to the file verbatim. Used for
    /// human-readable config files (YAML, TOML, JSON, env files,
    /// shell scripts, ...).
    Text,

    /// `payload` is base64-encoded and is decoded before being
    /// written. Used for binary files that can't safely live in a
    /// JSON string.
    Binary,
}

impl PreviewConfig {
    /// Default TTL in seconds when neither `ttl_mins` nor `ttl_secs` is set.
    pub const DEFAULT_TTL_SECS: u64 = 3600; // 1 hour

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

        for (idx, config_mount) in self.config_mounts.iter().enumerate() {
            match (&config_mount.payload, &config_mount.from_file) {
                (Some(_), Some(_)) => {
                    return Err(ConfigError::Conflict(format!(
                        "`feature.preview.config_mounts[{idx}]` has both `payload` and `from_file` \
                         set; specify exactly one"
                    )));
                }
                (None, None) => {
                    return Err(ConfigError::Conflict(format!(
                        "`feature.preview.config_mounts[{idx}]` must specify either `payload` or \
                         `from_file`"
                    )));
                }
                (Some(payload), None) => {
                    let Some(kind) = &config_mount.r#type else {
                        return Err(ConfigError::Conflict(format!(
                            "`feature.preview.config_mounts[{idx}].type` is required when `payload` \
                             is set"
                        )));
                    };
                    if *kind == ConfigMountType::Binary {
                        BASE64_STANDARD.decode(payload).map_err(|err| {
                            ConfigError::InvalidValue {
                                name: format!("feature.preview.config_mounts[{idx}].payload")
                                    .into(),
                                provided: payload.clone(),
                                error: Box::new(err),
                            }
                        })?;
                    }
                }
                (None, Some(_)) => {
                    if config_mount.r#type.is_some() {
                        return Err(ConfigError::Conflict(format!(
                            "`feature.preview.config_mounts[{idx}]` has both `from_file` and `type` set; `type` is resolved automatically when `from_file` is used and should not be specified."
                        )));
                    }
                }
            }
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
        analytics.add("config_mounts", self.config_mounts.len() as u32);
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
            config_mounts: vec![],
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

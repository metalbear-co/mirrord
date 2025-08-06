//! Config for the `copy target` feature.
//!
//! [`CopyTargetFileConfig`] does follow the pattern of other
//! [`feature`](crate::feature) configs by not implementing
//! [`MirrordToggleableConfig`](crate::util::MirrordToggleableConfig). The reason for this is that
//! [`ToggleableConfig`](crate::util::ToggleableConfig) is enabled by default. This config should be
//! disabled unless explicitly enabled.

use mirrord_analytics::CollectAnalytics;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{ConfigContext, FromMirrordConfig, MirrordConfig, Result};

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[serde(untagged, deny_unknown_fields)]
pub enum CopyTargetFileConfig {
    Simple(bool),
    Advanced {
        enabled: Option<bool>,
        scale_down: Option<bool>,
        exclude_containers: Option<Vec<String>>,
        exclude_init_containers: Option<Vec<String>>,
    },
}

impl Default for CopyTargetFileConfig {
    fn default() -> Self {
        Self::Simple(false)
    }
}

impl MirrordConfig for CopyTargetFileConfig {
    type Generated = CopyTargetConfig;

    fn generate_config(self, _context: &mut ConfigContext) -> Result<Self::Generated> {
        let res = match self {
            Self::Simple(enabled) => Self::Generated {
                enabled,
                scale_down: false,
                exclude_containers: vec![],
                exclude_init_containers: vec![],
            },
            Self::Advanced {
                enabled,
                scale_down,
                exclude_containers,
                exclude_init_containers,
            } => Self::Generated {
                enabled: enabled.unwrap_or(true),
                scale_down: scale_down.unwrap_or_default(),
                exclude_containers: exclude_containers.unwrap_or_default(),
                exclude_init_containers: exclude_init_containers.unwrap_or_default(),
            },
        };

        Ok(res)
    }
}

impl FromMirrordConfig for CopyTargetConfig {
    type Generator = CopyTargetFileConfig;
}

/// Allows the user to target a pod created dynamically from the original [`target`](#target).
/// The new pod inherits most of the original target's specification, e.g. labels.
///
/// ```json
/// {
///   "feature": {
///     "copy_target": {
///       "scale_down": true,
///       "exclude_containers": ["my-container"],
///       "exclude_init_containers": ["my-init-container"]
///     }
///   }
/// }
/// ```
///
/// ```json
/// {
///   "feature": {
///     "copy_target": true
///   }
/// }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CopyTargetConfig {
    pub enabled: bool,

    /// ### feature.copy_target.scale_down {#feature-copy_target-scale_down}
    ///
    /// If this option is set, mirrord will scale down the target deployment to 0 for the time
    /// the copied pod is alive.
    ///
    /// This option is compatible only with deployment targets.
    /// ```json
    ///     {
    ///       "scale_down": true
    ///     }
    /// ```
    pub scale_down: bool,

    /// ### feature.copy_target.exclude_containers {#feature-copy_target-exclude_containers}
    ///
    /// Set a list of containers to be ignored by copy_target
    pub exclude_containers: Vec<String>,

    /// ### feature.copy_target.exclude_init_containers {#feature-copy_target-exclude_init_containers}
    ///
    /// Set a list of init containers to be ignored by copy_target
    pub exclude_init_containers: Vec<String>,
}

impl CollectAnalytics for &CopyTargetConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("enabled", self.enabled);
        analytics.add("scale_down", self.scale_down);
    }
}

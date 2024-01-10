//! Config for the `copy target` feature. [`CopyTargetFileConfig`] does follow the pattern of other
//! [`feature`](crate::feature) configs by not implementing
//! [`MirrordToggleableConfig`](crate::util::MirrordToggleableConfig). The reason for this is that
//! [`ToggleableConfig`](crate::util::ToggleableConfig) is enabled by default. This config should be
//! disabled unless explicitly enabled.

use mirrord_analytics::CollectAnalytics;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::config::{ConfigContext, FromMirrordConfig, MirrordConfig, Result};

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[serde(untagged)]
pub enum CopyTargetFileConfig {
    Simple(bool),
    Advanced { scale_down: Option<bool> },
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
            },
            Self::Advanced { scale_down } => Self::Generated {
                enabled: true,
                scale_down: scale_down.unwrap_or_default(),
            },
        };

        Ok(res)
    }
}

impl FromMirrordConfig for CopyTargetConfig {
    type Generator = CopyTargetFileConfig;
}

/// Allows the user to target a pod created dynamically from the orignal [`target`](#target).
/// The new pod inherits most of the original target's specification, e.g. labels.
///
/// ```json
/// {
///   "feature": {
///     "copy_target": {
///       "scale_down": true
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
#[derive(Clone, Debug)]
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
}

impl CollectAnalytics for &CopyTargetConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("enabled", self.enabled);
        analytics.add("scale_down", self.scale_down);
    }
}

use std::path::PathBuf;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Configuration for mirrord for CI.
///
/// ```json
/// {
///   "ci": {
///     "output_dir": "/tmp/mirrord/",
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[config(map_to = "CiFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct CiConfig {
    /// ### ci.output_dir{#ci-output_dir}
    ///
    /// Path to a directory where `mirrord ci` will flush application's stdout and stderr.
    ///
    /// Defaults to `/tmp/mirrord/`.
    pub output_dir: Option<PathBuf>,
}

impl CollectAnalytics for &CiConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("stdio_dir", self.output_dir.is_some());
    }
}

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
///     "info_dir": "/tmp/mirrord/",
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[config(map_to = "CiFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct CiConfig {
    /// ### ci.info_dir{#ci-info_dir}
    ///
    /// `mirrord ci` commands creates some temporary files (e.g. a file for the output of `stdio`),
    /// and you can specify the directory where these files will be created here.
    ///
    /// Defaults to `/tmp/mirrord/{binary-name}-{timestamp}-{random-name}`
    pub info_dir: Option<PathBuf>,
}

impl CollectAnalytics for &CiConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("stdio_dir", self.info_dir.is_some());
    }
}

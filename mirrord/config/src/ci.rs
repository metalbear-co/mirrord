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
///     "lock_timeout_ms": 1000
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "CiFileConfig", derive = "JsonSchema, Serialize")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct CiConfig {
    /// ### ci.output_dir {#ci-output_dir}
    ///
    /// Path to a directory where `mirrord ci` will flush application's stdout and stderr.
    ///
    /// Defaults to `/tmp/mirrord/`.
    pub output_dir: Option<PathBuf>,

    /// ### ci.lock_timeout_ms {#ci-lock_timeout_ms}
    ///
    /// How long, in milliseconds, `mirrord ci start` and `mirrord ci container` wait before failing
    /// when another `mirrord ci` command is using the file where mirrord records the processes that
    /// `mirrord ci stop` ends.
    ///
    /// Sessions started at the same time take turns recording their processes in this file, so the
    /// later sessions wait for the earlier ones. `mirrord ci stop` holds the file while it signals
    /// processes and removes containers, so raise this value if you start sessions while
    /// `mirrord ci stop` runs. `mirrord ci stop` itself doesn't wait: it fails at once if another
    /// command holds the file.
    ///
    /// Defaults to `1000` milliseconds.
    #[config(default = 1000)]
    pub lock_timeout_ms: u64,
}

impl CollectAnalytics for &CiConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("stdio_dir", self.output_dir.is_some());
    }
}

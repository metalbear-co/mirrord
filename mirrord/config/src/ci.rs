use std::path::PathBuf;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Configuration for mirrord for CI.
///
/// Logs from the app are sent to `/dev/null` when using `mirrord ci start`. You can pipe them
/// to a file by setting one of the `stdio` file paths (note that the file doesn't have to exist,
/// mirrord will take care of creating it).
///
/// ```json
/// {
///   "ci": {
///     "stdout_file": "/tmp/mirrord-ci-logs/stdout",
///     "stderr_file": "/tmp/mirrord-ci-logs/stderr"
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[config(map_to = "CiFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct CiConfig {
    /// ### ci.stdout_file {#ci-stdout_file}
    ///
    /// Pipe `stdout` to the file specified here.
    pub stdout_file: Option<PathBuf>,

    /// ### ci.stderr_file {#ci-stderr_file}
    ///
    /// Pipe `stderr` to the file specified here.
    ///
    /// If you're running mirrord with logs enabled, this will contain your app `stderr` logs, and
    /// mirrord logs.
    pub stderr_file: Option<PathBuf>,
}

impl CollectAnalytics for &CiConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("stdout_file", self.stdout_file.is_some());
        analytics.add("stderr_file", self.stdout_file.is_some());
    }
}

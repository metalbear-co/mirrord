use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Controls how many times, and how often mirrord retries its initial Kubernetes API requests (e.g.
/// for resolving the target or connecting to the mirrord Operator).
///
/// If you're having cluster connectivity issues when **starting** mirrord, consider increasing
/// [`max_retries`](#startup_retry-max_retries) and changing both
/// [`min_ms`](#startup_retry-min_ms) and [`max_ms`](#startup_retry-max_ms) to have mirrord retry
/// some of its initial Kubernetes API requests.
///
/// ```json
/// {
///   "startup_retry": {
///     "min_ms": 500,
///     "max_ms": 5000,
///     "max_retries": 2,
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "StartupRetryFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct StartupRetryConfig {
    /// ### startup_retry.min_ms {#startup_retry-min_ms}
    ///
    /// Sets the min interval (in milliseconds) of retries for Kubernetes API requests made by
    /// mirrord during startup (e.g. for resolving the target or connecting to the mirrord
    /// Operator).
    ///
    /// Defaults to `500` milliseconds.
    #[config(default = 500)]
    pub min_ms: u64,

    /// ### startup_retry.max_ms {#startup_retry-max_ms}
    ///
    /// Sets the max interval (in milliseconds) of retries for Kubernetes API requests made by
    /// mirrord during startup (e.g. for resolving the target or connecting to the mirrord
    /// Operator).
    ///
    /// Defaults to `5000` milliseconds.
    #[config(default = 5000)]
    pub max_ms: u64,

    /// ### startup_retry.max_retries {#startup_retry-max_retries}
    ///
    /// Sets the max amount of retries for Kubernetes API requests made by mirrord during startup
    /// (e.g. for resolving the target or connecting to the mirrord Operator).
    ///
    /// If you want to **disable** request retries, set this value to `0`.
    ///
    /// Defaults to `2`.
    #[config(default = 2)]
    pub max_retries: u32,
}

impl CollectAnalytics for &StartupRetryConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("min_ms", self.min_ms);
        analytics.add("max_ms", self.max_ms);
        analytics.add("max_attempts", self.max_retries);
    }
}

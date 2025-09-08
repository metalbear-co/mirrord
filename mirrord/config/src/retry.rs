use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "StartupRetryFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct StartupRetryConfig {
    #[config(default = 500)]
    pub min_ms: u64,

    /// ## startup_retries_interval_ms {#root-startup_retries_interval_ms}
    ///
    /// Sets the interval (in milliseconds) between mirrord startup retries during its startup, for
    /// cluster operations, such as searching for the target pod, connecting to the
    /// mirrord-operator, creating the mirrord-agent.
    ///
    /// If you are having cluster connectivity issues when starting mirrord, setting this config
    /// and [`startup_retries_max_attempts`](#root-startup_retries_max_attempts) may help.
    #[config(default = 5000)]
    pub max_ms: u64,

    /// ## startup_retries_max_attempts {#root-startup_retries_max_attempts}
    ///
    /// Sets the max amount of retries that mirrord will try to perform during its startup, for
    /// cluster operations, such as searching for the target pod, connecting to the
    /// mirrord-operator, creating the mirrord-agent.
    ///
    /// If you are having cluster connectivity issues when starting mirrord, setting this config
    /// and [`startup_retries_interval_ms`](#root-startup_retries_interval_ms) may help.
    #[config(default = 2)]
    pub max_attempts: u32,
}

impl CollectAnalytics for &StartupRetryConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("min_ms", self.min_ms);
        analytics.add("max_ms", self.max_ms);
        analytics.add("max_attempts", self.max_attempts);
    }
}

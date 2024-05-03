use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::config::source::MirrordConfigSource;

/// mirrord Experimental features.
/// This shouldn't be used unless someone from MetalBear/mirrord tells you to.
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "ExperimentalFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct ExperimentalConfig {
    /// ## experimental {#fexperimental-tcp_ping4_mock}
    ///
    /// https://github.com/metalbear-co/mirrord/issues/2421#issuecomment-2093200904
    #[config(default = true)]
    pub tcp_ping4_mock: bool,
}

impl CollectAnalytics for &ExperimentalConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("tcp_ping4_mock", self.tcp_ping4_mock);
    }
}

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
    /// ## _experimental_ tcp_ping4_mock {#experimental-tcp_ping4_mock}
    ///
    /// <https://github.com/metalbear-co/mirrord/issues/2421#issuecomment-2093200904>
    #[config(default = true)]
    pub tcp_ping4_mock: bool,

    /// ## _experimental_ readlink {#experimental-readlink}
    ///
    /// Enables the `readlink` hook.
    #[config(default = false)]
    pub readlink: bool,

    /// ## _experimental_ trust_any_certificate {#experimental-trust_any_certificate}
    ///
    /// Enables trusting any certificate on macOS, useful for <https://github.com/golang/go/issues/51991#issuecomment-2059588252>
    #[config(default = false)]
    pub trust_any_certificate: bool,

    /// ## _experimental_ disable_exec_hooks {#experimental-disable_exec_hooks}
    ///
    /// Disables exec hooks on Linux. Disabling Linux hooks will cause issues when the application
    /// shares sockets with child commands (e.g Python web servers with reload), but may solve
    /// other issues.
    #[config(default = false)]
    pub disable_exec_hooks_linux: bool,
}

impl CollectAnalytics for &ExperimentalConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("tcp_ping4_mock", self.tcp_ping4_mock);
        analytics.add("readlink", self.readlink);
        analytics.add("trust_any_certificate", self.trust_any_certificate);
        analytics.add("disable_exec_hooks_linux", self.disable_exec_hooks_linux);
    }
}

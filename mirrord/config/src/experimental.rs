use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// mirrord Experimental features.
/// This shouldn't be used unless someone from MetalBear/mirrord tells you to.
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "ExperimentalFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct ExperimentalConfig {
    /// ### _experimental_ tcp_ping4_mock {#experimental-tcp_ping4_mock}
    ///
    /// <https://github.com/metalbear-co/mirrord/issues/2421#issuecomment-2093200904>
    #[config(default = true)]
    pub tcp_ping4_mock: bool,

    /// ### _experimental_ readlink {#experimental-readlink}
    ///
    /// DEPRECATED, WILL BE REMOVED
    #[config(default = false)]
    pub readlink: bool,

    /// ### _experimental_ trust_any_certificate {#experimental-trust_any_certificate}
    ///
    /// Enables trusting any certificate on macOS, useful for <https://github.com/golang/go/issues/51991#issuecomment-2059588252>
    #[config(default = false)]
    pub trust_any_certificate: bool,

    /// ### _experimental_ enable_exec_hooks_linux {#experimental-enable_exec_hooks_linux}
    ///
    /// Enables exec hooks on Linux. Enable Linux hooks can fix issues when the application
    /// shares sockets with child commands (e.g Python web servers with reload),
    /// but the feature is not stable and may cause other issues.
    #[config(default = true)]
    pub enable_exec_hooks_linux: bool,

    /// ### _experimental_ hide_ipv6_interfaces {#experimental-hide_ipv6_interfaces}
    ///
    /// Enables `getifaddrs` hook that removes IPv6 interfaces from the list returned by libc.
    #[config(default = false)]
    pub hide_ipv6_interfaces: bool,

    /// ### _experimental_ disable_reuseaddr {#experimental-disable_reuseaddr}
    ///
    /// Disables the `SO_REUSEADDR` socket option on sockets that mirrord steals/mirrors.
    /// On macOS the application can use the same address many times but then we don't steal it
    /// correctly. This probably should be on by default but we want to gradually roll it out.
    /// <https://github.com/metalbear-co/mirrord/issues/2819>
    /// This option applies only on macOS.
    #[config(default = false)]
    pub disable_reuseaddr: bool,

    /// ### _experimental_ use_dev_null {#experimental-use_dev_null}
    ///
    /// Uses /dev/null for creating local fake files (should be better than using /tmp)
    #[config(default = true)]
    pub use_dev_null: bool,

    /// ### _experimental_ readonly_file_buffer {#experimental-readonly_file_buffer}
    ///
    /// Sets buffer size for readonly remote files (in bytes, for example 4096).
    /// If set, such files will be read in chunks and buffered locally.
    /// This improves performace when the user application reads data in small portions.
    ///
    /// Setting to 0 disables file buffering.
    ///
    /// <https://github.com/metalbear-co/mirrord/issues/2069>
    #[config(default = 128000)]
    pub readonly_file_buffer: u64,

    /// ### _experimental_ idle_local_http_connection_timeout {#experimental-idle_local_http_connection_timeout}
    ///
    /// Sets a timeout for idle local HTTP connections (in milliseconds).
    ///
    /// HTTP requests stolen with a filter are delivered to the local application
    /// from a HTTP connection made from the local machine. Once a request is delivered,
    /// the connection is cached for some time, so that it can be reused to deliver
    /// the next request.
    ///
    /// This timeout determines for how long such connections are cached.
    ///
    /// Set to 0 to disable caching local HTTP connections (connections will be dropped as soon as
    /// the request is delivered).
    #[config(default = 3000)]
    pub idle_local_http_connection_timeout: u64,
}

impl CollectAnalytics for &ExperimentalConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("tcp_ping4_mock", self.tcp_ping4_mock);
        analytics.add("readlink", self.readlink);
        analytics.add("trust_any_certificate", self.trust_any_certificate);
        analytics.add("enable_exec_hooks_linux", self.enable_exec_hooks_linux);
        analytics.add("hide_ipv6_interfaces", self.hide_ipv6_interfaces);
        analytics.add("disable_reuseaddr", self.disable_reuseaddr);
        analytics.add("readonly_file_buffer", self.readonly_file_buffer);
        analytics.add(
            "idle_local_http_connection_timeout",
            self.idle_local_http_connection_timeout,
        );
    }
}

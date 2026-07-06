use std::path::PathBuf;

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
    ///
    /// Defaults to `true` in OSS.
    /// Defaults to `false` in mfT.
    #[config(default = None)]
    pub disable_reuseaddr: Option<bool>,

    /// ### _experimental_ use_dev_null {#experimental-use_dev_null}
    ///
    /// Uses /dev/null for creating local fake files (should be better than using /tmp)
    #[config(default = true)]
    pub use_dev_null: bool,

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
    ///
    /// Defaults to 3000ms.
    #[config(default = 3000)]
    pub idle_local_http_connection_timeout: u64,

    /// ### _experimental_ ignore_system_proxy_config {#experimental-ignore_system_proxy_config}
    ///
    /// Disables any system wide proxy configuration for affecting the running application.
    #[config(default = false)]
    pub ignore_system_proxy_config: bool,

    /// ### _experimental_ browser_extension_config {#experimental-browser_extension_config}
    ///
    /// mirrord will open a URL for initiating mirrord browser extension to
    /// automatically inject HTTP header that matches the HTTP filter configured in
    /// `feature.network.incoming.http_filter.header_filter`.
    #[config(default = false)]
    pub browser_extension_config: bool,

    /// ### _experimental_ sip_log_destination {#experimental-sip_log_destination}
    ///
    /// Writes basic fork-safe SIP patching logs to a destination file.
    /// Useful for seeing the state of SIP when `stdout` may be affected by another process.
    #[config(default = None)]
    pub sip_log_destination: Option<PathBuf>,

    /// ### _experimental_non_blocking_tcp_connect {#experimental-non_blocking_tcp_connect}
    ///
    /// Enables better support for outgoing connections using
    /// non-blocking TCP sockets.
    ///
    /// Due to technical reasons, enabling this causes the existence
    /// of the internal proxy acting as a TCP proxy observable to the
    /// application. Notable consequences of this include:
    ///
    /// 1. `getsockname(2)` will always return a localhost address,
    /// 2. `connect(2)` will succeed immediately, before the remote side of the connection is
    ///    established. If the remote side of the connection fails, the connection to the app will
    ///    be closed after the fact.
    ///
    /// Essentially your application has to assume that it might be
    /// sitting behind a TCP proxy. Most "standard" apps (e.g. HTTP
    /// clients) should be unaffected, but custom implementations of
    /// other protocols may not.
    ///
    /// DEPRECATED, WILL BE REMOVED
    #[config(
        default = true,
        deprecated = "`non_blocking_tcp_connect` is deprecated and enabled by default."
    )]
    pub non_blocking_tcp_connect: bool,

    /// ### _experimental_ dlopen_cgo {#experimental-dlopen_cgo}
    ///
    /// Useful when the user's application loads a c-shared golang library dynamically.
    ///
    /// Defaults to `false`.
    #[config(default = false)]
    pub dlopen_cgo: bool,

    /// ### _experimental_ latency {#experimental-latency}
    ///
    /// Configuration for adding artificial latency to outgoing network operations.
    ///
    /// DEPRECATED, WILL BE REMOVED
    /// Please use the mirrord chaos feature instead.
    #[config(
        nested,
        deprecated = "`latency` is deprecated. Please use the mirrord chaos feature instead."
    )]
    pub latency: LatencyConfig,

    /// ### _experimental_ applev {#experimental-applev}
    ///
    /// Configuration for inspecting and modifying apple variables. macOS only.
    pub applev: Option<AppleVariablesConfig>,

    /// ### _experimental_ sip_utils {#experimental-sip_utils}
    ///
    /// Extract pre-built SIP utility binaries into `~/.mirrord/binaries` on macOS and uses
    /// them in place of SIP-patching the originals.
    /// This shouldn't be used unless someone from MetalBear/mirrord tells you to.
    ///
    /// DEPRECATED, WILL BE REMOVED
    #[config(
        default = true,
        deprecated = "`sip_utils` is deprecated and enabled by default."
    )]
    pub sip_utils: bool,

    /// ### _experimental_ go_cgo_stack_switch {#go_cgo_stack_switch}
    ///
    /// use cgo's depth-based stack restore when switching back from the g0 stack in the Go 1.25+
    /// syscall hook.
    #[config(default = false)]
    pub go_cgo_stack_switch: bool,

    /// ### _experimental_ go_asmcgocall {#go_asmcgocall}
    ///
    /// On x86-64, route the Go 1.25+ syscall hook through the Go runtime's own
    /// `runtime.asmcgocall` for the switch to and from the `g0` system stack, instead of the
    /// hand-rolled assembly switch. This mirrors what the arm64 hook already does and avoids
    /// corrupting the scheduler state (`g.sched`) of goroutines blocked in a syscall, which
    /// can crash cgo-heavy Go programs. Takes precedence over `go_cgo_stack_switch` when both
    /// are set. No effect on arm64, which always uses `runtime.asmcgocall`.
    #[config(default = false)]
    pub go_asmcgocall: bool,
}

impl CollectAnalytics for &ExperimentalConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("tcp_ping4_mock", self.tcp_ping4_mock);
        analytics.add("trust_any_certificate", self.trust_any_certificate);
        analytics.add("enable_exec_hooks_linux", self.enable_exec_hooks_linux);
        analytics.add("hide_ipv6_interfaces", self.hide_ipv6_interfaces);
        if let Some(disable_reuseaddr) = self.disable_reuseaddr {
            analytics.add("disable_reuseaddr", disable_reuseaddr);
        }
        analytics.add(
            "idle_local_http_connection_timeout",
            self.idle_local_http_connection_timeout,
        );
        analytics.add("browser_extension_config", self.browser_extension_config);
        analytics.add("non_blocking_tcp_connect", self.non_blocking_tcp_connect);
        analytics.add("dlopen_cgo", self.dlopen_cgo);
        analytics.add("latency_transmit_delay", self.latency.transmit_delay);
        analytics.add("latency_receive_delay", self.latency.receive_delay);
        analytics.add("applev", self.applev.is_some());
        analytics.add("sip_utils", self.sip_utils);
        analytics.add("go_cgo_stack_switch", self.go_cgo_stack_switch);
        analytics.add("go_asmcgocall", self.go_asmcgocall);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct AppleVariablesConfig {}

/// Configuration for adding artificial latency to outgoing network operations.
/// Useful for testing application behavior under network delay conditions.
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "LatencyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct LatencyConfig {
    /// ### _experimental_ latency.transmit_delay {#experimental-latency-transmit_delay}
    ///
    /// Delay in milliseconds for outgoing send operations (Layer → Agent).
    ///
    /// Defaults to `0` (no delay).
    #[config(default = 0)]
    pub transmit_delay: u64,

    /// ### _experimental_ latency.receive_delay {#experimental-latency-receive_delay}
    ///
    /// Delay in milliseconds for outgoing receive operations (Agent → Layer).
    ///
    /// Defaults to `0` (no delay).
    #[config(default = 0)]
    pub receive_delay: u64,
}

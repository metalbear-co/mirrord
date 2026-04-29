use core::net::{IpAddr, Ipv4Addr};
use std::{env, fs};

use mirrord_config::LayerConfig;

use crate::config::ContainerRuntime;

/// Modifies the [`LayerConfig`] for WSL environments when running the `mirrord container`.
///
/// - Only when the runtime is Docker, and the user has not specified neither the
///   `external_proxy.host_ip` nor the `container.override_host_ip`, so we use the default docker
///   values.
pub(super) fn adjust_container_config_for_wsl(runtime: ContainerRuntime, config: &mut LayerConfig) {
    if is_wsl()
        && config.external_proxy.host_ip.is_none()
        && config.container.override_host_ip.is_none()
        && matches!(runtime, ContainerRuntime::Docker)
    {
        config.external_proxy.host_ip = Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        config.container.override_host_ip =
            Some("192.168.65.254".parse().expect("Valid hardcoded IP"));
    }
}

/// Detect if we're running inside Windows Subsystem for Linux (WSL).
///
/// This function checks several indicators to determine if the current environment
/// is running under WSL:
///
/// 1. Checks for WSL-specific environment variables
/// 2. Reads `/proc/version` for Microsoft signature
/// 3. Checks for WSL interop socket
///
/// - returns: `true` if WSL is detected, `false` otherwise.
fn is_wsl() -> bool {
    // Check for WSL environment variables
    env::var("WSL_DISTRO_NAME").is_ok()
        || env::var("WSLENV").is_ok()
        || env::var("WSL_INTEROP").is_ok()
        // Check /proc/version for Microsoft signature
        || fs::read_to_string("/proc/version")
            .is_ok_and(|version_info| version_info.to_lowercase().contains("microsoft"))
        // Check for WSL interop socket (WSL2)
        || fs::metadata("/run/WSL").is_ok()
}

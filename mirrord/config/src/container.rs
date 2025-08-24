use std::net::IpAddr;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

static DEFAULT_CLI_IMAGE: &str = concat!(
    "ghcr.io/metalbear-co/mirrord-cli:",
    env!("CARGO_PKG_VERSION")
);

/// Unstable: `mirrord container` command specific config.
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[config(map_to = "ContainerFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct ContainerConfig {
    /// ### container.cli_image {#container-cli_image}
    ///
    /// Tag of the `mirrord-cli` image you want to use.
    ///
    /// Defaults to `"ghcr.io/metalbear-co/mirrord-cli:<cli version>"`.
    #[config(default = DEFAULT_CLI_IMAGE)]
    pub cli_image: String,

    /// ### container.cli_extra_args {#container-cli_extra_args}
    ///
    /// Any extra args to use when creating the sidecar mirrord-cli container.
    ///
    /// This is useful when you want to use portforwarding, passing `-p local:container` won't work
    /// for main command but adding them here will work
    /// ```json
    /// {
    ///   "container": {
    ///     "cli_extra_args": ["-p", "local:container"]
    ///   }
    /// }
    /// ```
    #[config(default)]
    pub cli_extra_args: Vec<String>,

    /// ### container.cli_prevent_cleanup {#container-cli_extra_args}
    ///
    /// Don't add `--rm` to sidecar command to prevent cleanup.
    #[config(default)]
    pub cli_prevent_cleanup: bool,

    /// ### container.cli_image_lib_path {#container-cli_image}
    ///
    /// Path of the mirrord-layer lib inside the specified mirrord-cli image.
    ///
    /// Defaults to `"/opt/mirrord/lib/libmirrord_layer.so"`.
    pub cli_image_lib_path: Option<String>,

    /// ### container.override_host_ip {#container-override_host_ip}
    ///
    /// Allows to override the IP address for the internal proxy to use
    /// when connecting to the host machine from within the container.
    ///
    /// ```json5
    /// {
    ///   "container": {
    ///     "override_host_ip": "172.17.0.1" // usual resolution of value from `host.docker.internal`
    ///   }
    /// }
    /// ```
    ///
    /// This should be useful if your host machine is exposed with a different IP address than the
    /// one bound as host.
    ///
    /// - If you're running inside WSL, and encountering problems, try setting
    ///   `external_proxy.host_ip` T `0.0.0.0`, and this to the internal container runtime address
    ///   (for docker, this  would be what `host.docker.internal` resolved to, which by default is
    ///   `192.168.65.254`). You can find this ip by resolving it from inside a running container,
    ///   e.g. `docker run --rm -it {image-with-nslookup} nslookup host.docker.internal`
    pub override_host_ip: Option<IpAddr>,

    /// ### container.platform {#container-platform}
    ///
    /// Platform specification for the target container (e.g., "linux/amd64", "linux/arm64").
    ///
    /// When specified, the target container will run with this platform, while the internal proxy
    /// container will still run on the native platform and contain both architectures (x64/arm64).
    /// The LD_PRELOAD will automatically use the correct architecture.
    ///
    /// ```json
    /// {
    ///   "container": {
    ///     "platform": "linux/amd64"
    ///   }
    /// }
    /// ```
    pub platform: Option<String>,
}

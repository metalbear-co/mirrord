use std::path::PathBuf;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

// static DEFAULT_CLI_IMAGE: &str = concat!(
//     "ghcr.io/metalbear-co/mirrord-cli:",
//     env!("CARGO_PKG_VERSION")
// );

static DEFAULT_CLI_IMAGE: &str = concat!("ghcr.io/metalbear-co/mirrord-cli:", "3.133.1");

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
    #[config(default = PathBuf::from("/opt/mirrord/lib/libmirrord_layer.so"))]
    pub cli_image_lib_path: PathBuf,
}

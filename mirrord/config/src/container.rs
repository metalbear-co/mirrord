use std::path::PathBuf;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::source::MirrordConfigSource;

static DEFAULT_CLI_IMAGE: &str = concat!(
    "ghcr.io/metalbear-co/mirrord-cli:",
    env!("CARGO_PKG_VERSION")
);

/// Unstable: `mirrord container` command specific config.
#[derive(MirrordConfig, Clone, Debug, Serialize)]
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

    /// ### container.cli_image_lib_path {#container-cli_image}
    ///
    /// Path of the mirrord-layer lib inside the specified mirrord-cli image.
    ///
    /// Defaults to `"/opt/mirrord/lib/libmirrord_layer.so"`.
    #[config(default = PathBuf::from("/opt/mirrord/lib/libmirrord_layer.so"))]
    pub cli_image_lib_path: PathBuf,
}

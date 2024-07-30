use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::source::MirrordConfigSource;

#[derive(MirrordConfig, Clone, Debug, Serialize)]
#[config(map_to = "ExternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct ContainerConfig {
    /// ### container.cli_image {#container-cli_image}
    ///
    /// Tag of the `mirrord-cli` image you want to use.
    #[config(default = concat!("ghcr.io/metalbear-co/mirrord-cli:", env!("CARGO_PKG_VERSION")))]
    pub cli_image: String,

    /// ### container.cli_image_lib_path {#container-cli_image}
    ///
    /// Path of the mirrord lib.
    #[config(default = "/opt/mirrord/lib/libmirrord_layer.so")]
    pub cli_image_lib_path: String,
}

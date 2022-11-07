use mirrord_config_derive::MirrordConfig2;
use schemars::JsonSchema;

use crate::{
    config::source::MirrordConfigSource, env::EnvConfig, fs::FsConfig, network::NetworkConfig,
};

/// Configuration for mirrord features.
///
/// For more information, check the [technical reference](https://mirrord.dev/docs/reference/)
/// of the feature.
///
/// ## Examples
///
/// - Exclude "SECRET" environment variable, enable read-write file operations, mirror network
///   traffic (default option), and generate a crash report (if there is any crash):
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature]
/// fs = "write"
/// capture_error_trace = true
///
/// [feature.env]
/// exclude = "SECRET"
/// ```
///
/// - Include only "DATABASE_URL", and "PORT" environment variables, enable read-write file
///   operations (only for `.txt` files), and enable both incoming and outgoing network traffic
///   (mirror):
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.env]
/// include = "DATABASE_URL;PORT"
///
/// [feature.fs]
/// mode = "write"
/// include = "^.*\.txt$"
///
/// [feature.network]
/// incoming = "mirror" # default, included here for effect
///
/// [feature.network.outgoing]
/// tcp = true
/// udp = true
/// ```
#[derive(MirrordConfig2, Default, PartialEq, Eq, Clone, Debug)]
#[config(
    map_to = "FeatureFileConfig",
    derive = "Default,PartialEq,Eq,JsonSchema"
)]
pub struct FeatureConfig {
    /// Controls the environment variables feature, see [`EnvFileConfig`].
    ///
    /// For more information, check the environment variables
    /// [technical reference](https://mirrord.dev/docs/reference/env/).
    #[config(nested, toggleable, from_default)]
    pub env: EnvConfig,

    /// Controls the file operations feature, see [`FsUserConfig`].
    ///
    /// For more information, check the file operations
    /// [technical reference](https://mirrord.dev/docs/reference/fileops/).
    #[config(nested, toggleable, from_default)]
    pub fs: FsConfig,

    /// Controls the network feature, see [`NetworkFileConfig`].
    ///
    /// For more information, check the network traffic
    /// [technical reference](https://mirrord.dev/docs/reference/traffic/).
    #[config(nested, toggleable, from_default)]
    pub network: NetworkConfig,

    /// Controls the crash reporting feature.
    ///
    /// With this feature enabled, mirrord generates a nice crash report log.
    #[config(env = "MIRRORD_CAPTURE_ERROR_TRACE", default = "false")]
    pub capture_error_trace: bool,
}

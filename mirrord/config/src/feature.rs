use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::source::MirrordConfigSource, env::EnvConfig, fs::FsConfig, network::NetworkConfig,
};

/// # feature
///
/// Configuration for mirrord features.
///
/// For more information, check the [technical reference](https://mirrord.dev/docs/reference/)
/// of the feature.
///
/// ## Types
///
/// ```json
/// {
///   "env": EnvConfig,
///   "fs": FsConfig,
///   "network": NetworkConfig,
///   "capture_error_trace": bool,
/// }
/// ```
///
/// ## Sample
///
/// - `config.json`:
///
/// ```
/// {
///   "feature": {
///     "env": {
///       "include": "DATABASE_USER;PUBLIC_ENV",
///       "exclude": "DATABASE_PASSWORD;SECRET_ENV",
///       "overrides": {
///         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
///         "LOCAL_BEAR": "panda"
///       }
///     },
///     "fs": {
///       "mode": "write",
///       "read_write": ".+\.json" ,
///       "read_only": [ ".+\.yaml", ".+important-file\.txt" ],
///       "local": [ ".+\.js", ".+\.mjs" ]
///     }
///   }
/// }
/// ```
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
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "FeatureFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct FeatureConfig {
    /// Controls the environment variables feature, see [`EnvConfig`].
    ///
    /// For more information, check the environment variables
    /// [technical reference](https://mirrord.dev/docs/reference/env/).
    #[config(nested, toggleable)]
    pub env: EnvConfig,

    /// Controls the file operations feature, see [`FsConfig`].
    ///
    /// For more information, check the file operations
    /// [technical reference](https://mirrord.dev/docs/reference/fileops/).
    #[config(nested, toggleable)]
    pub fs: FsConfig,

    /// Controls the network feature, see [`NetworkConfig`].
    ///
    /// For more information, check the network traffic
    /// [technical reference](https://mirrord.dev/docs/reference/traffic/).
    #[config(nested, toggleable)]
    pub network: NetworkConfig,

    /// Controls the crash reporting feature.
    ///
    /// With this feature enabled, mirrord generates a nice crash report log.
    #[config(env = "MIRRORD_CAPTURE_ERROR_TRACE", default = false)]
    pub capture_error_trace: bool,
}

pub mod deprecated;
pub mod from_env;
pub mod source;
pub mod unstable;

use thiserror::Error;

// rustdoc-stripper-ignore-next
/// Error that would be returned from [MirrordConfig::generate_config]
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("invalid target provided `{0}`!")]
    InvalidTarget(String),

    #[error("value for {1:?} not provided in {0:?} (env override {2:?})")]
    ValueNotProvided(&'static str, &'static str, Option<&'static str>),

    #[error("value {0:?} for {1:?} is invalid.")]
    InvalidValue(String, &'static str),

    #[error("mirrord-config: IO operation failed with `{0}`")]
    Io(#[from] std::io::Error),

    #[error("mirrord-config: `{0}`!")]
    SerdeJson(#[from] serde_json::Error),

    #[error("mirrord-config: `{0}`!")]
    Toml(#[from] toml::de::Error),

    #[error("mirrord-config: `{0}`!")]
    SerdeYaml(#[from] serde_yaml::Error),

    #[error("mirrord-config: Unsupported configuration file format!")]
    UnsupportedFormat,

    #[error("Invalid FS mode `{0}`!")]
    InvalidFsMode(String),
}

pub type Result<T, E = ConfigError> = std::result::Result<T, E>;

/// Main configuration creation trait of mirrord-config
pub trait MirrordConfig {
    /// The resulting struct you plan on using in the rest of your code
    type Generated;

    /// Load configuration from all sources and output as [Self::Generated]
    fn generate_config(self) -> Result<Self::Generated>;
}

impl<T> MirrordConfig for Option<T>
where
    T: MirrordConfig + Default,
{
    type Generated = T::Generated;

    fn generate_config(self) -> Result<Self::Generated> {
        self.unwrap_or_default().generate_config()
    }
}

/// Lookup trait for accessing type implementing [MirrordConfig] from [MirrordConfig::Generated]
// rustdoc-stripper-ignore-next-stop
pub trait FromMirrordConfig {
    type Generator: MirrordConfig;
}

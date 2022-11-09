pub mod default_value;
pub mod from_env;
pub mod source;

use thiserror::Error;

/// Error that would be returned from [MirrordConfig::generate_config]
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("invalid target provided `{0}`!")]
    InvalidTarget(String),
    #[error("value for {1:?} not provided in {0:?} (env override {2:?})")]
    ValueNotProvided(&'static str, &'static str, Option<&'static str>),
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
pub trait FromMirrordConfig {
    type Generator: MirrordConfig;
}

pub mod default_value;
pub mod deprecated;
pub mod from_env;
pub mod source;
pub mod unstable;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("invalid target provided `{0}`!")]
    InvalidTarget(String),

    #[error("value for {1:?} not provided in {0:?} (env override {2:?})")]
    ValueNotProvided(&'static str, &'static str, Option<&'static str>),

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
}

pub type Result<T, E = ConfigError> = std::result::Result<T, E>;

pub trait MirrordConfig {
    type Generated;

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

pub mod default_value;
pub mod from_env;
pub mod source;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("invalid target provided `{0}`!")]
    InvalidTarget(String),
    #[error("value for {1:?} not provided in {0:?} (env override {2:?})")]
    ValueNotProvided(&'static str, &'static str, Option<&'static str>),
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

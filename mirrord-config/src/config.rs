pub mod default_value;
pub mod from_env;
pub mod source;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("couldn't find value for {0:?}")]
    ValueNotProvided(String),
}

use thiserror::Error;

pub trait MirrordConfig {
    type Generated;

    fn generate_config(self) -> Result<Self::Generated, ConfigError>;
}

impl<T> MirrordConfig for Option<T>
where
    T: MirrordConfig + Default,
{
    type Generated = T::Generated;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        self.unwrap_or_default().generate_config()
    }
}

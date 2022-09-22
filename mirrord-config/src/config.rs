pub mod default_value;
pub mod from_env;
pub mod source;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("value for {1:?} not provided in {0:?} (env override {2:?})")]
    ValueNotProvided(&'static str, &'static str, Option<&'static str>),
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

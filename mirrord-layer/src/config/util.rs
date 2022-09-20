use std::{slice::Join, str::FromStr};

use serde::Deserialize;
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

pub trait MirrordFlaggedConfig: MirrordConfig + Default {
    fn enabled_config() -> Result<Self::Generated, ConfigError> {
        Self::default().generate_config()
    }

    fn disabled_config() -> Result<Self::Generated, ConfigError>;
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(untagged)]
pub enum FlagField<T> {
    Enabled(bool),
    Config(T),
}

impl<T> Default for FlagField<T> {
    fn default() -> Self {
        FlagField::<T>::Enabled(true)
    }
}

impl<T> MirrordConfig for FlagField<T>
where
    T: MirrordFlaggedConfig,
{
    type Generated = T::Generated;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        match self {
            FlagField::Enabled(true) => T::enabled_config(),
            FlagField::Enabled(false) => T::disabled_config(),
            FlagField::Config(inner) => inner.generate_config(),
        }
    }
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(untagged)]
pub enum VecOrSingle<T> {
    Single(T),
    Multiple(Vec<T>),
}

impl<T> VecOrSingle<T> {
    pub fn join<Separator>(self, sep: Separator) -> <[T] as Join<Separator>>::Output
    where
        [T]: Join<Separator>,
    {
        match self {
            VecOrSingle::Single(val) => [val].join(sep),
            VecOrSingle::Multiple(vals) => vals.join(sep),
        }
    }
}

impl<T> FromStr for VecOrSingle<T>
where
    T: FromStr,
{
    type Err = T::Err;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        let mut multiple = Vec::new();

        for part in val.split(';') {
            multiple.push(T::from_str(part)?);
        }

        Ok(VecOrSingle::Multiple(multiple))
    }
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("couldn't find value for {0:?}")]
    ValueNotProvided(String),
}

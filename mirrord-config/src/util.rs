use std::{slice::Join, str::FromStr};

use serde::Deserialize;

use crate::config::{ConfigError, MirrordConfig};

pub trait MirrordToggleableConfig: MirrordConfig + Default {
    fn enabled_config() -> Result<Self::Generated, ConfigError> {
        Self::default().generate_config()
    }

    fn disabled_config() -> Result<Self::Generated, ConfigError>;
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug)]
#[serde(untagged)]
pub enum ToggleableConfig<T> {
    Enabled(bool),
    Config(T),
}

impl<T> Default for ToggleableConfig<T> {
    fn default() -> Self {
        ToggleableConfig::<T>::Enabled(true)
    }
}

impl<T> MirrordConfig for ToggleableConfig<T>
where
    T: MirrordToggleableConfig,
{
    type Generated = T::Generated;

    fn generate_config(self) -> Result<Self::Generated, ConfigError> {
        match self {
            ToggleableConfig::Enabled(true) => T::enabled_config(),
            ToggleableConfig::Enabled(false) => T::disabled_config(),
            ToggleableConfig::Config(inner) => inner.generate_config(),
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

    pub fn to_vec(self) -> Vec<T> {
        match self {
            VecOrSingle::Single(val) => vec![val],
            VecOrSingle::Multiple(vals) => vals,
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

        if multiple.len() == 1 {
            return Ok(VecOrSingle::Single(multiple.remove(0)));
        }

        Ok(VecOrSingle::Multiple(multiple))
    }
}

#[cfg(test)]
pub mod testing {
    use std::{
        env,
        env::VarError,
        panic,
        panic::{RefUnwindSafe, UnwindSafe},
        sync::{LazyLock, Mutex},
    };

    static SERIAL_TEST: LazyLock<Mutex<()>> = LazyLock::new(Default::default);

    /// Sets environment variables to the given value for the duration of the closure.
    /// Restores the previous values when the closure completes or panics, before unwinding the
    /// panic.
    pub fn with_env_vars<F>(kvs: Vec<(&str, Option<&str>)>, closure: F)
    where
        F: Fn() + UnwindSafe + RefUnwindSafe,
    {
        let guard = SERIAL_TEST.lock().unwrap();
        let mut old_kvs: Vec<(&str, Result<String, VarError>)> = Vec::new();
        for (k, v) in kvs {
            let old_v = env::var(k);
            old_kvs.push((k, old_v));
            match v {
                None => env::remove_var(k),
                Some(v) => env::set_var(k, v),
            }
        }

        match panic::catch_unwind(|| {
            closure();
        }) {
            Ok(_) => {
                for (k, v) in old_kvs {
                    reset_env(k, v);
                }
            }
            Err(err) => {
                for (k, v) in old_kvs {
                    reset_env(k, v);
                }
                drop(guard);
                panic::resume_unwind(err);
            }
        };
    }

    fn reset_env(k: &str, old: Result<String, VarError>) {
        if let Ok(v) = old {
            env::set_var(k, v);
        } else {
            env::remove_var(k);
        }
    }
}

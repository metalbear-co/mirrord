use std::{collections::HashSet, fmt, hash::Hash, marker::PhantomData, slice::Join, str::FromStr};

use schemars::JsonSchema;
use serde::{
    de::{self, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};

use crate::config::{ConfigContext, ConfigError, FromMirrordConfig, MirrordConfig, Result};

pub trait MirrordToggleableConfig: MirrordConfig + Default {
    fn enabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        Self::default().generate_config(context)
    }

    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError>;
}

#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
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

    fn generate_config(self, context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        match self {
            ToggleableConfig::Enabled(true) => T::enabled_config(context),
            ToggleableConfig::Enabled(false) => T::disabled_config(context),
            ToggleableConfig::Config(inner) => inner.generate_config(context),
        }
    }
}

impl<T> FromMirrordConfig for ToggleableConfig<T>
where
    T: FromMirrordConfig,
{
    type Generator = T::Generator;
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum VecOrSingle<T> {
    Single(T),
    Multiple(Vec<T>),
}

impl<T> VecOrSingle<T> {
    pub fn as_slice(&self) -> &[T] {
        match self {
            Self::Single(v) => std::slice::from_ref(v),
            Self::Multiple(v) => v.as_slice(),
        }
    }

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

    pub fn len(&self) -> usize {
        match self {
            VecOrSingle::Single(_) => 1,
            VecOrSingle::Multiple(vals) => vals.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            VecOrSingle::Single(_) => false,
            VecOrSingle::Multiple(vals) => vals.is_empty(),
        }
    }
}

impl<T: Hash + Eq> From<VecOrSingle<T>> for HashSet<T> {
    fn from(value: VecOrSingle<T>) -> Self {
        match value {
            VecOrSingle::Single(val) => {
                let mut set = HashSet::with_capacity(1);
                set.insert(val);
                set
            }
            VecOrSingle::Multiple(vals) => vals.into_iter().collect(),
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

pub fn string_or_struct_option<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de> + FromStr<Err = ConfigError>,
    D: Deserializer<'de>,
{
    // This is a Visitor that forwards string types to T's `FromStr` impl and
    // forwards map types to T's `Deserialize` impl. The `PhantomData` is to
    // keep the compiler from complaining about T being an unused generic type
    // parameter. We need T in order to know the Value type for the Visitor
    // impl.
    struct StringOrStruct<T>(PhantomData<fn() -> T>);

    impl<'de, T> Visitor<'de> for StringOrStruct<T>
    where
        T: Deserialize<'de> + FromStr<Err = ConfigError>,
    {
        type Value = Option<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("string or map or none")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            FromStr::from_str(value)
                .map(T::into)
                .map_err(|err| de::Error::custom(err))
        }

        fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            // `MapAccessDeserializer` is a wrapper that turns a `MapAccess`
            // into a `Deserializer`, allowing it to be used as the input to T's
            // `Deserialize` implementation. T then deserializes itself using
            // the entries from the map visitor.
            Deserialize::deserialize(de::value::MapAccessDeserializer::new(map)).map(T::into)
        }
    }

    deserializer.deserialize_any(StringOrStruct(PhantomData))
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

    /// <!--${internal}-->
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

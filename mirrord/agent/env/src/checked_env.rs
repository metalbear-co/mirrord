use std::{
    any,
    convert::Infallible,
    fmt,
    marker::PhantomData,
    net::{AddrParseError, IpAddr, SocketAddr},
    os::unix::ffi::OsStrExt,
    str::{FromStr, Utf8Error},
};

#[cfg(feature = "k8s-openapi")]
use k8s_openapi::api::core::v1::EnvVar;
use thiserror::Error;

/// Type of an environment variable value.
pub trait EnvValue: Sized {
    /// Error that can occur when producing the value representation.
    type IntoReprError;
    /// Error that can occur when reading the value from the representation.
    type FromReprError;

    /// Produces a representation for the given value.
    fn as_repr(&self) -> Result<String, Self::IntoReprError>;

    /// Reads a value from the given representation.
    fn from_repr(repr: &[u8]) -> Result<Self, Self::FromReprError>;
}

/// Convenience trait to mark value types that can be converted with [`ToString`] and [`FromStr`].
///
/// Implementors are provided with a blanket implementation of [`EnvValue`].
pub trait StoredAsString: ToString + FromStr {}

impl<T: StoredAsString> EnvValue for T {
    type IntoReprError = Infallible;
    type FromReprError = ParseEnvError<T::Err>;

    fn as_repr(&self) -> Result<String, Self::IntoReprError> {
        Ok(self.to_string())
    }

    fn from_repr(repr: &[u8]) -> Result<Self, Self::FromReprError> {
        let as_str = std::str::from_utf8(repr)?;
        as_str.parse().map_err(ParseEnvError::ParseError)
    }
}

/// Errors that can occur when reading a [`StoredAsString`] environment variable value.
#[derive(Error, Debug)]
pub enum ParseEnvError<E> {
    Utf8Error(#[from] Utf8Error),
    ParseError(#[source] E),
}

/// An environment variable with strict value type checking.
pub struct CheckedEnv<V: EnvValue> {
    /// Name of the variable.
    pub name: &'static str,
    value_type: PhantomData<fn() -> V>,
}

impl<V: EnvValue> CheckedEnv<V> {
    /// Creates a new instance.
    ///
    /// All instances should be kept together in [`super::envs`].
    pub(crate) const fn new(name: &'static str) -> Self {
        Self {
            name,
            value_type: PhantomData,
        }
    }

    /// Produces an [`EnvVar`] spec, using the given value.
    #[cfg(feature = "k8s-openapi")]
    pub fn try_as_k8s_spec(self, value: &V) -> Result<EnvVar, V::IntoReprError> {
        let repr = value.as_repr()?;

        Ok(EnvVar {
            name: self.name.into(),
            value: Some(repr),
            value_from: None,
        })
    }

    /// Reads this variable's value from the process environment.
    pub fn try_from_env(self) -> Result<Option<V>, V::FromReprError> {
        match std::env::var_os(self.name) {
            Some(repr) => {
                let value = V::from_repr(repr.as_bytes())?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
}

impl<V: EnvValue<IntoReprError = Infallible>> CheckedEnv<V> {
    /// Convenience method for producing an [`EnvVar`] spec
    /// when producing value representation cannot fail.
    #[cfg(feature = "k8s-openapi")]
    pub fn as_k8s_spec(self, value: &V) -> EnvVar {
        let Ok(env) = self.try_as_k8s_spec(value);
        env
    }
}

impl<V> CheckedEnv<V>
where
    V: EnvValue + Default + fmt::Debug,
    V::FromReprError: fmt::Debug,
{
    /// Convenience method for getting this variable's value.
    ///
    /// Reads this variable's value from the process environment.
    /// If this variable is missing or its value is malformed, returns a [`Default`].
    pub fn from_env_or_default(self) -> V {
        match self.try_from_env() {
            Ok(None) => Default::default(),
            Ok(Some(value)) => value,
            Err(error) => {
                let default_value = V::default();

                tracing::error!(
                    ?error,
                    ?default_value,
                    ?self,
                    "Failed to read an environment variable, value is malformed. Using a default value.",
                );

                default_value
            }
        }
    }
}

impl<V: EnvValue> fmt::Debug for CheckedEnv<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedEnv")
            .field("name", &self.name)
            .field("value_type", &any::type_name::<V>())
            .finish()
    }
}

impl<V: EnvValue> fmt::Display for CheckedEnv<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

impl<V: EnvValue> Clone for CheckedEnv<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V: EnvValue> Copy for CheckedEnv<V> {}

impl<V: EnvValue> PartialEq for CheckedEnv<V> {
    fn eq(&self, other: &Self) -> bool {
        self.name.eq(other.name)
    }
}

impl<V: EnvValue> Eq for CheckedEnv<V> {}

impl StoredAsString for bool {}

impl StoredAsString for u32 {}

impl StoredAsString for SocketAddr {}

impl StoredAsString for String {}

impl EnvValue for Vec<IpAddr> {
    type IntoReprError = Infallible;
    type FromReprError = ParseEnvError<AddrParseError>;

    fn as_repr(&self) -> Result<String, Self::IntoReprError> {
        Ok(self
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","))
    }

    fn from_repr(repr: &[u8]) -> Result<Self, Self::FromReprError> {
        let as_str = std::str::from_utf8(repr)?;

        as_str
            .split(',')
            .map(|item| item.parse::<IpAddr>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(ParseEnvError::ParseError)
    }
}

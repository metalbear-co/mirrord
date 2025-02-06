use std::{
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
    fn into_repr(&self) -> Result<String, Self::IntoReprError>;

    /// Reads a value from the given representation.
    fn from_repr(repr: &[u8]) -> Result<Self, Self::FromReprError>;
}

pub trait StoredAsString: fmt::Display + FromStr {}

impl<T: StoredAsString> EnvValue for T {
    type IntoReprError = Infallible;
    type FromReprError = ParseEnvError<T::Err>;

    fn into_repr(&self) -> Result<String, Self::IntoReprError> {
        Ok(self.to_string())
    }

    fn from_repr(repr: &[u8]) -> Result<Self, Self::FromReprError> {
        let as_str = std::str::from_utf8(repr).map_err(ParseEnvError::Utf8Error)?;
        as_str.parse().map_err(ParseEnvError::ParseError)
    }
}

/// Errors that can occur when reading an environment variable value from the representation using
/// [`StringRepr`] or [`CommaSeparatedRepr`].
#[derive(Error, Debug)]
pub enum ParseEnvError<E> {
    Utf8Error(#[source] Utf8Error),
    ParseError(#[source] E),
}

/// An environment variable with strict value type checking.
#[derive(Clone, Copy, PartialEq, Eq)]
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
        let repr = V::into_repr(value)?;

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

impl CheckedEnv<bool> {
    /// Convenience method for checking whether this variable is set.
    ///
    /// For variables with [`bool`] values.
    ///
    /// Returns `true` if the variable is present and set to true.
    /// Returns `false` if the variable is missing or set to some other value.
    pub fn is_set(self) -> bool {
        self.try_from_env().ok().flatten().unwrap_or_default()
    }
}

impl<V: EnvValue> fmt::Debug for CheckedEnv<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

impl<V: EnvValue> fmt::Display for CheckedEnv<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

impl StoredAsString for bool {}

impl StoredAsString for u32 {}

impl StoredAsString for SocketAddr {}

impl EnvValue for String {
    type IntoReprError = Infallible;
    type FromReprError = Utf8Error;

    fn into_repr(&self) -> Result<String, Self::IntoReprError> {
        Ok(self.clone())
    }

    fn from_repr(repr: &[u8]) -> Result<Self, Self::FromReprError> {
        std::str::from_utf8(repr).map(From::from)
    }
}

impl EnvValue for Vec<IpAddr> {
    type IntoReprError = Infallible;
    type FromReprError = ParseEnvError<AddrParseError>;

    fn into_repr(&self) -> Result<String, Self::IntoReprError> {
        Ok(self
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","))
    }

    fn from_repr(repr: &[u8]) -> Result<Self, Self::FromReprError> {
        let as_str = std::str::from_utf8(repr).map_err(ParseEnvError::Utf8Error)?;

        as_str
            .split(',')
            .map(|item| item.parse::<IpAddr>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(ParseEnvError::ParseError)
    }
}

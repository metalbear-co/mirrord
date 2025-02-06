use std::{
    convert::Infallible,
    fmt,
    marker::PhantomData,
    os::unix::ffi::OsStrExt,
    str::{FromStr, Utf8Error},
};

#[cfg(feature = "k8s-openapi")]
use k8s_openapi::api::core::v1::EnvVar;
use thiserror::Error;

/// A representation of an environment variable value.
pub trait EnvRepr {
    /// Type of the value, e.g `u32`.
    type Value;
    /// Error that can occur when producing the value representation.
    type IntoReprError;
    /// Error that can occur when reading the value from the representation.
    type FromReprError;

    /// Produces a representation for the given value.
    fn into_repr(value: &Self::Value) -> Result<String, Self::IntoReprError>;

    /// Reads a value from the given representation.
    fn from_repr(repr: &[u8]) -> Result<Self::Value, Self::FromReprError>;
}

/// Implementation of [`EnvRepr`] that uses [`fmt::Display`] and [`FromStr`] to handle conversions.
pub struct StringRepr<T>(PhantomData<fn() -> T>);

/// Errors that can occur when reading an environment variable value from the representation using
/// [`StringRepr`] or [`CommaSeparatedRepr`].
#[derive(Error, Debug)]
pub enum ParseEnvError<E> {
    Utf8Error(#[source] Utf8Error),
    ParseError(#[source] E),
}

impl<T> EnvRepr for StringRepr<T>
where
    T: fmt::Display + FromStr,
{
    type Value = T;
    type IntoReprError = Infallible;
    type FromReprError = ParseEnvError<T::Err>;

    fn into_repr(value: &T) -> Result<String, Self::IntoReprError> {
        Ok(value.to_string())
    }

    fn from_repr(repr: &[u8]) -> Result<T, Self::FromReprError> {
        let as_str = std::str::from_utf8(repr).map_err(ParseEnvError::Utf8Error)?;
        as_str.parse().map_err(ParseEnvError::ParseError)
    }
}

/// Implementation of [`EnvRepr`] for vectors of values.
///
/// Uses [`fmt::Display`] and [`FromStr`] to handle conversion of each value.
/// Final representation is a comma-separated list.
pub struct CommaSeparatedRepr<T>(PhantomData<fn() -> T>);

impl<T> EnvRepr for CommaSeparatedRepr<T>
where
    T: fmt::Display + FromStr,
{
    type Value = Vec<T>;
    type IntoReprError = Infallible;
    type FromReprError = ParseEnvError<T::Err>;

    fn from_repr(repr: &[u8]) -> Result<Self::Value, Self::FromReprError> {
        let as_str = std::str::from_utf8(repr).map_err(ParseEnvError::Utf8Error)?;

        as_str
            .split(',')
            .map(|item| item.parse::<T>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(ParseEnvError::ParseError)
    }

    fn into_repr(value: &Self::Value) -> Result<String, Self::IntoReprError> {
        Ok(value
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","))
    }
}

/// An environment variable with strict value type checking.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CheckedEnv<R: EnvRepr> {
    /// Name of the variable.
    pub name: &'static str,
    repr: PhantomData<fn() -> R>,
}

impl<R: EnvRepr> CheckedEnv<R> {
    /// Creates a new instance.
    ///
    /// All instances should be kept together in [`super::envs`].
    pub(crate) const fn new(name: &'static str) -> Self {
        Self {
            name,
            repr: PhantomData,
        }
    }

    /// Produces an [`EnvVar`] spec, using the given value.
    #[cfg(feature = "k8s-openapi")]
    pub fn try_as_k8s_spec(self, value: &R::Value) -> Result<EnvVar, R::IntoReprError> {
        let repr = R::into_repr(value)?;

        Ok(EnvVar {
            name: self.name.into(),
            value: Some(repr),
            value_from: None,
        })
    }

    /// Reads this variable's value from the process environment.
    pub fn try_from_env(self) -> Result<Option<R::Value>, R::FromReprError> {
        match std::env::var_os(self.name) {
            Some(repr) => {
                let value = R::from_repr(repr.as_bytes())?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
}

impl<R: EnvRepr<IntoReprError = Infallible>> CheckedEnv<R> {
    /// Convenience method for producing an [`EnvVar`] spec
    /// when producing value representation cannot fail.
    #[cfg(feature = "k8s-openapi")]
    pub fn as_k8s_spec(self, value: &R::Value) -> EnvVar {
        let Ok(env) = self.try_as_k8s_spec(value);
        env
    }
}

impl<R: EnvRepr<Value = bool>> CheckedEnv<R> {
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

impl<R: EnvRepr> fmt::Debug for CheckedEnv<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

impl<R: EnvRepr> fmt::Display for CheckedEnv<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

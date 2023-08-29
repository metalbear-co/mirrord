use std::{marker::PhantomData, str::FromStr};

use super::ConfigContext;
use crate::config::{source::MirrordConfigSource, ConfigError, Result};

#[derive(Clone)]
pub struct FromEnv<T>(&'static str, PhantomData<T>);

impl<T> FromEnv<T> {
    pub fn new(env: &'static str) -> Self {
        FromEnv(env, PhantomData::<T>)
    }
}

impl<T> MirrordConfigSource for FromEnv<T>
where
    T: FromStr,
{
    type Value = T;

    fn source_value(self, _context: &mut ConfigContext) -> Option<Result<Self::Value>> {
        std::env::var(self.0).ok().map(|var| {
            var.parse()
                .map_err(|_| ConfigError::InvalidValue(var.to_string(), self.0))
        })
    }
}

/// <!--${internal}-->
/// This is the same as `FromEnv` but doesn't discard the error returned from parse.
///
/// This is for parsing `Target`. I (A.H) couldn't find any better way to do this since you can't
/// do specialization on associated types.
#[derive(Clone)]
pub struct FromEnvWithError<T>(&'static str, PhantomData<T>);

impl<T> FromEnvWithError<T> {
    pub fn new(env: &'static str) -> Self {
        FromEnvWithError(env, PhantomData::<T>)
    }
}

impl<T> MirrordConfigSource for FromEnvWithError<T>
where
    T: FromStr<Err = ConfigError>,
{
    type Value = T;

    fn source_value(self, _context: &mut ConfigContext) -> Option<Result<Self::Value>> {
        std::env::var(self.0).ok().map(|var| var.parse())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::util::testing::with_env_vars;

    #[rstest]
    fn basic() {
        with_env_vars(vec![("TEST_VALUE", Some("13"))], || {
            let mut cfg_context = ConfigContext::default();
            let value = FromEnv::<i32>::new("TEST_VALUE");

            assert_eq!(value.source_value(&mut cfg_context).unwrap().unwrap(), 13);
        });
        let mut cfg_context = ConfigContext::default();
        let value = FromEnv::<i32>::new("TEST_VALUE");
        assert!(value.source_value(&mut cfg_context).is_none());
    }
}

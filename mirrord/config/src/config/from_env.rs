use std::{marker::PhantomData, str::FromStr};

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

    fn source_value(self) -> Option<Result<Self::Value>> {
        std::env::var(self.0).ok().map(|var| {
            var.parse()
                .map_err(|_| ConfigError::InvalidValue(var.to_string(), self.0))
        })
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
            let value = FromEnv::<i32>::new("TEST_VALUE");

            assert_eq!(value.source_value().unwrap().unwrap(), 13);
        });
        let value = FromEnv::<i32>::new("TEST_VALUE");
        assert!(value.source_value().is_none());
    }
}

use std::{marker::PhantomData, str::FromStr};

use super::ConfigError;
use crate::config::{source::MirrordConfigSource, Result};

#[derive(Clone)]
pub struct DefaultValue<T>(&'static str, PhantomData<T>);

impl<T> DefaultValue<T> {
    pub fn new(env: &'static str) -> Self {
        DefaultValue(env, PhantomData::<T>)
    }
}

impl<T> MirrordConfigSource for DefaultValue<T>
where
    T: FromStr,
{
    type Value = T;

    fn source_value(self) -> Option<Result<T>> {
        Some(
            self.0
                .parse()
                .map_err(|_| ConfigError::InvalidDefaultValue(self.0)),
        )
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn basic() {
        let value = DefaultValue::<i32>::new("13");

        assert!(matches!(value.source_value(), Some(Ok(13))));
    }
}

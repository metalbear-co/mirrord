use std::{marker::PhantomData, str::FromStr};

use crate::config::source::MirrordConfigSource;

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
    type Result = T;

    fn source_value(self) -> Option<T> {
        std::env::var(self.0).ok().and_then(|var| var.parse().ok())
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn basic() {
        let value = FromEnv::<i32>::new("FROM_ENV_TEST_VALUE");

        assert_eq!(value.clone().source_value(), None);

        std::env::set_var("FROM_ENV_TEST_VALUE", "13");

        assert_eq!(value.source_value(), Some(13));
    }
}

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
    use rstest::rstest;

    use super::*;
    use crate::util::testing::with_env_vars;

    #[rstest]
    #[case(None, None)]
    #[case(Some("13"), Some(13))]
    fn basic(#[case] env: Option<&str>, #[case] expect: Option<i32>) {
        with_env_vars(vec![("TEST_VALUE", env)], || {
            let value = FromEnv::new("TEST_VALUE");

            assert_eq!(value.source_value(), expect);
        });
    }
}

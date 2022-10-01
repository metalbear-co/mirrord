pub trait MirrordConfigSource: Sized {
    type Result;

    fn source_value(self) -> Option<Self::Result>;
}

impl<T> MirrordConfigSource for Option<T>
where
    T: Clone,
{
    type Result = T;

    fn source_value(self) -> Option<Self::Result> {
        self
    }
}

impl<R, T> MirrordConfigSource for (T,)
where
    T: MirrordConfigSource<Result = R>,
{
    type Result = T::Result;

    fn source_value(self) -> Option<Self::Result> {
        self.0.source_value()
    }
}

impl<R, T, V> MirrordConfigSource for (T, V)
where
    T: MirrordConfigSource<Result = R>,
    V: MirrordConfigSource<Result = R>,
{
    type Result = T::Result;

    fn source_value(self) -> Option<Self::Result> {
        self.0.source_value().or_else(|| self.1.source_value())
    }
}

impl<R, T, V, K> MirrordConfigSource for (T, V, K)
where
    T: MirrordConfigSource<Result = R>,
    V: MirrordConfigSource<Result = R>,
    K: MirrordConfigSource<Result = R>,
{
    type Result = T::Result;

    fn source_value(self) -> Option<Self::Result> {
        self.0
            .source_value()
            .or_else(|| self.1.source_value())
            .or_else(|| self.2.source_value())
    }
}

impl<R, T, V, K, P> MirrordConfigSource for (T, V, K, P)
where
    T: MirrordConfigSource<Result = R>,
    V: MirrordConfigSource<Result = R>,
    K: MirrordConfigSource<Result = R>,
    P: MirrordConfigSource<Result = R>,
{
    type Result = T::Result;

    fn source_value(self) -> Option<Self::Result> {
        self.0
            .source_value()
            .or_else(|| self.1.source_value())
            .or_else(|| self.2.source_value())
            .or_else(|| self.3.source_value())
    }
}

impl<R, T, V, K, P, F> MirrordConfigSource for (T, V, K, P, F)
where
    T: MirrordConfigSource<Result = R>,
    V: MirrordConfigSource<Result = R>,
    K: MirrordConfigSource<Result = R>,
    P: MirrordConfigSource<Result = R>,
    F: MirrordConfigSource<Result = R>,
{
    type Result = T::Result;

    fn source_value(self) -> Option<Self::Result> {
        self.0
            .source_value()
            .or_else(|| self.1.source_value())
            .or_else(|| self.2.source_value())
            .or_else(|| self.3.source_value())
            .or_else(|| self.4.source_value())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::{default_value::DefaultValue, from_env::FromEnv},
        util::testing::with_env_vars,
    };

    #[rstest]
    #[case(None, 10)]
    #[case(Some("13"), 13)]
    fn basic(#[case] env: Option<&str>, #[case] expect: i32) {
        with_env_vars(vec![("TEST_VALUE", env)], || {
            let val = (FromEnv::new("TEST_VALUE"), None, DefaultValue::new("10"));

            assert_eq!(val.source_value(), Some(expect));
        });
    }
}

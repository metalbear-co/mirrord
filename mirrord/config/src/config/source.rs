use super::context::ConfigContext;
use crate::config::Result;

pub trait MirrordConfigSource: Sized {
    type Value;

    fn source_value(self, context: &mut ConfigContext) -> Option<Result<Self::Value>>;

    fn layer<L>(self, layer_fn: impl Fn(Self) -> L) -> L
    where
        L: MirrordConfigSource<Value = Self::Value>,
    {
        layer_fn(self)
    }

    fn or<T: MirrordConfigSource<Value = Self::Value>>(self, fallback: T) -> Or<Self, T> {
        Or::new(self, fallback)
    }
}

#[derive(Clone)]
pub struct Or<A, B>(A, B);

impl<A, B> Or<A, B>
where
    A: MirrordConfigSource,
    B: MirrordConfigSource<Value = A::Value>,
{
    fn new(first: A, fallback: B) -> Self {
        Or(first, fallback)
    }
}

impl<A, B> MirrordConfigSource for Or<A, B>
where
    A: MirrordConfigSource,
    B: MirrordConfigSource<Value = A::Value>,
{
    type Value = A::Value;

    fn source_value(self, context: &mut ConfigContext) -> Option<Result<Self::Value>> {
        self.0
            .source_value(context)
            .or_else(|| self.1.source_value(context))
    }
}

impl<V> MirrordConfigSource for Option<V>
where
    V: Clone,
{
    type Value = V;

    fn source_value(self, _context: &mut ConfigContext) -> Option<Result<Self::Value>> {
        self.map(Ok)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::config::from_env::FromEnv;

    #[rstest]
    #[case(None, 10)]
    #[case(Some("13"), 13)]
    fn basic(#[case] env: Option<&str>, #[case] outcome: i32) {
        let mut cfg_context = ConfigContext::default()
            .override_env_opt("TEST_VALUE", env)
            .override_env("FALLBACK", "10")
            .strict_env(true);
        let val = FromEnv::<i32>::new("TEST_VALUE")
            .or(None)
            .or(FromEnv::new("FALLBACK"));

        assert_eq!(
            val.source_value(&mut cfg_context).unwrap().unwrap(),
            outcome
        );
    }
}

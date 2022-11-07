use std::{marker::PhantomData, str::FromStr};

use crate::config::source::MirrordConfigSource;

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
    type Result = T;

    fn source_value(self) -> Option<T> {
        self.0.parse().ok()
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn basic() {
        let value = DefaultValue::<i32>::new("13");

        assert_eq!(value.source_value(), Some(13));
    }
}

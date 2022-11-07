use std::marker::PhantomData;

use crate::config::source::MirrordConfigSource;

#[derive(Clone)]
pub struct FromDefault<T>(PhantomData<T>);

impl<T> FromDefault<T> {
    pub fn new() -> Self {
        FromDefault(PhantomData::<T>)
    }
}

impl<T> MirrordConfigSource for FromDefault<T>
where
    T: Default,
{
    type Result = T;

    fn source_value(self) -> Option<T> {
        Some(Default::default())
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn basic() {
        let value = FromDefault::<i32>::new();

        assert_eq!(value.source_value(), Some(0));
    }
}

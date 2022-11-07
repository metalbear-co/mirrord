use std::marker::PhantomData;

use crate::config::source::MirrordConfigSource;

#[derive(Clone, Default)]
pub struct FromDefault<T>(PhantomData<T>);

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
        let value = FromDefault::<i32>::default();

        assert_eq!(value.source_value(), Some(0));
    }
}

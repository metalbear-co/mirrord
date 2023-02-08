use crate::config::{source::MirrordConfigSource, Result};

#[derive(Clone)]
pub struct Unstable<T>(&'static str, &'static str, T);

impl<T> Unstable<T> {
    pub fn new(container: &'static str, field: &'static str, inner: T) -> Self {
        Unstable(container, field, inner)
    }
}

impl<T> MirrordConfigSource for Unstable<T>
where
    T: MirrordConfigSource,
{
    type Value = T::Value;

    fn source_value(self) -> Option<Result<Self::Value>> {
        self.2.source_value().map(|result| {
            tracing::warn!(
                "Warning: field {}.{} is marked as unstable. Please note API may change",
                self.0,
                self.1
            );
            result
        })
    }
}

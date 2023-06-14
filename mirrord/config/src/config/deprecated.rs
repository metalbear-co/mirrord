use crate::config::{source::MirrordConfigSource, Result};

#[derive(Clone)]
pub struct Deprecated<T>(String, T);

impl<T> Deprecated<T> {
    pub fn new(message: &'static str, inner: T) -> Self {
        Deprecated(message.to_owned(), inner)
    }
}

impl<T> MirrordConfigSource for Deprecated<T>
where
    T: MirrordConfigSource,
{
    type Value = T::Value;

    fn source_value(self) -> Option<Result<Self::Value>> {
        self.1.source_value().map(|result| {
            tracing::warn!("{}", self.0);
            result
        })
    }
}

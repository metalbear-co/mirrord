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

    fn source_value(self, warnings: &mut Vec<String>) -> Option<Result<Self::Value>> {
        self.1.source_value(warnings).map(|result| {
            warnings.push(format!("{}", self.0));
            result
        })
    }
}

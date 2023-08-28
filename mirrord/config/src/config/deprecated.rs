use crate::config::{source::MirrordConfigSource, ConfigContext, Result};

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

    fn source_value(self, context: &mut ConfigContext) -> Option<Result<Self::Value>> {
        self.1.source_value(context).map(|result| {
            context.add_warning(self.0.to_string());
            result
        })
    }
}

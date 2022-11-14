use crate::config::source::MirrordConfigSource;

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
    type Result = T::Result;

    fn source_value(self) -> Option<T::Result> {
        self.1.source_value().map(|result| {
            println!("{}", self.0);
            result
        })
    }
}

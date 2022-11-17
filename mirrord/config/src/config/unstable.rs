use crate::config::source::MirrordConfigSource;

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
    type Result = T::Result;

    fn source_value(self) -> Option<T::Result> {
        self.2.source_value().map(|result| {
            println!(
                "Warning: field {}.{} is marked as unstable. Please note API may change",
                self.0, self.1
            );
            result
        })
    }
}

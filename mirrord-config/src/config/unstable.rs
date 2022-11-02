use crate::config::source::MirrordConfigSource;

#[derive(Clone)]
pub struct Unstable<T>(T);

impl<T> From<T> for Unstable<T> {
    fn from(inner: T) -> Self {
        Unstable(inner)
    }
}

impl<T> MirrordConfigSource for Unstable<T>
where
    T: MirrordConfigSource,
{
    type Result = T::Result;

    fn source_value(self) -> Option<T::Result> {
        self.0.source_value().map(|result| {
            println!("Warning you are using a fieled marked unstable, Please note api may change");
            result
        })
    }
}

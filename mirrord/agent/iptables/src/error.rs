use std::{error::Error, fmt, io, sync::Arc};

pub struct IPTablesError(pub Box<dyn Error + Send + Sync + 'static>);

/// [`iptables`] produces errors that are not [`Send`].
impl From<Box<dyn Error>> for IPTablesError {
    fn from(value: Box<dyn Error>) -> Self {
        Self(value.to_string().into())
    }
}

impl From<io::Error> for IPTablesError {
    fn from(value: io::Error) -> Self {
        Self(Box::new(value))
    }
}

impl From<IPTablesError> for Arc<dyn Error + Send + Sync + 'static> {
    fn from(value: IPTablesError) -> Self {
        value.0.into()
    }
}

impl fmt::Debug for IPTablesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for IPTablesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for IPTablesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

pub type IPTablesResult<T, E = IPTablesError> = Result<T, E>;

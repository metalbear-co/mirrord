use std::{error::Error, fmt, io};

pub struct IPTablesError(pub Box<dyn Error>);

impl From<Box<dyn Error>> for IPTablesError {
    fn from(value: Box<dyn Error>) -> Self {
        Self(value)
    }
}

impl From<io::Error> for IPTablesError {
    fn from(value: io::Error) -> Self {
        Self(Box::new(value))
    }
}

impl From<String> for IPTablesError {
    fn from(value: String) -> Self {
        Self(value.into())
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

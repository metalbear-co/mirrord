//! Stand-in for `std::error::Report`, which is still gated behind the unstable `error_reporter`
//! feature. Shared by crates that log an error together with its [`std::error::Error::source`]
//! chain.

use std::fmt;

/// Formats an error together with its `source()` chain.
///
/// Mirrors the subset of `std::error::Report` that this workspace actually needs: a single-line
/// `error: cause: cause` form by default, or a multi-line `Caused by:` form via [`Self::pretty`].
pub struct Report<E> {
    error: E,
    pretty: bool,
}

impl<E> Report<E> {
    pub fn new(error: E) -> Self {
        Self {
            error,
            pretty: false,
        }
    }

    /// Enables multi-line output, with each cause in the source chain on its own indented line.
    pub fn pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }
}

impl<E> fmt::Display for Report<E>
where
    E: std::error::Error,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)?;

        let mut source = self.error.source();
        if self.pretty && source.is_some() {
            write!(f, "\n\nCaused by:")?;
        }
        while let Some(error) = source {
            if self.pretty {
                write!(f, "\n    {error}")?;
            } else {
                write!(f, ": {error}")?;
            }
            source = error.source();
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::fmt;

    use super::Report;

    #[derive(Debug)]
    struct Root;

    impl fmt::Display for Root {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "root error")
        }
    }

    impl std::error::Error for Root {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&Mid)
        }
    }

    #[derive(Debug)]
    struct Mid;

    impl fmt::Display for Mid {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "mid error")
        }
    }

    impl std::error::Error for Mid {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&Leaf)
        }
    }

    #[derive(Debug)]
    struct Leaf;

    impl fmt::Display for Leaf {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "leaf error")
        }
    }

    impl std::error::Error for Leaf {}

    #[test]
    fn single_line() {
        let report = Report::new(Root);
        assert_eq!(report.to_string(), "root error: mid error: leaf error");
    }

    #[test]
    fn pretty() {
        let report = Report::new(Root).pretty(true);
        assert_eq!(
            report.to_string(),
            "root error\n\nCaused by:\n    mid error\n    leaf error"
        );
    }

    #[test]
    fn no_source() {
        let report = Report::new(Leaf);
        assert_eq!(report.to_string(), "leaf error");
    }
}

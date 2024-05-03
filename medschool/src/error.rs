use thiserror::Error;

/// We just _eat_ some of these errors (turn them into `None`).
#[derive(Debug, Error)]
pub(crate) enum DocsError {
    /// Error for glob iteration.
    #[error("Glob error {0}")]
    Glob(#[from] glob::GlobError),

    /// Parsing glob pattern.
    #[error("Glob pattern {0}")]
    Pattern(#[from] glob::PatternError),

    /// IO issues we have when reading the source files or producing the `.md` file.
    #[error("IO error {0}")]
    IO(#[from] std::io::Error),

    /// Error when parsing the source files into `syn::File`.
    #[error("Parsing error {0}")]
    Parse(#[from] syn::Error),
}

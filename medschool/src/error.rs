//! Errors defined for medschool.

use thiserror::Error;

/// Errors that can happen when working with the documentation.
#[derive(Debug, Error)]
pub enum DocsError {
    /// Error for glob iteration.
    #[error(transparent)]
    Glob(#[from] glob::GlobError),

    /// No files found in the glob pattern.
    #[error("No files found in the glob pattern")]
    NoFiles,

    /// Parsing glob pattern.
    #[error(transparent)]
    Pattern(#[from] glob::PatternError),

    /// IO issues we have when reading the source files or producing the `.md` file.
    #[error("IO error {0}")]
    IO(#[from] std::io::Error),

    /// Error when parsing the source files into `syn::File`.
    #[error(transparent)]
    Parse(#[from] syn::Error),

    /// Error when parsing the source files into `syn::File`.
    #[error(transparent)]
    ParseInt(#[from] std::num::ParseIntError),
}

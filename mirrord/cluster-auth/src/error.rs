//! Errors of remote cluster connections.

use std::borrow::Cow;

use thiserror::Error;

/// A type-erased error, for a cause whose concrete type cannot be named at the point it is
/// stored - several unrelated failures reaching one variant, or a generic whose error type
/// the caller chooses. Prefer a concrete type whenever a field can only ever hold one.
///
/// Same type as `tower::BoxError`/`axum::BoxError`, spelled here so crates that have
/// neither can use the name too.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Names the operation that failed while keeping the failure itself reachable.
///
/// Use this instead of folding an error into a message (`format!("failed to spawn pg_dump:
/// {error}")`). The rendered text is identical when logged with [`std::error::Report`], but
/// the cause stays a real error: downcastable, and never truncated by an outer type whose
/// own `Display` only prints its own message.
#[derive(Debug, Error)]
#[error("{context}")]
pub struct WithContext {
    context: Cow<'static, str>,
    #[source]
    source: BoxError,
}

/// Attaches `context` to `source`, ready to be stored in a `Box<dyn Error>` field.
///
/// ```ignore
/// Command::new("pg_dump").spawn().map_err(|error| {
///     DumpError::DumpExecution(context("failed to spawn pg_dump", error))
/// })?;
/// ```
pub fn context(context: impl Into<Cow<'static, str>>, source: impl Into<BoxError>) -> BoxError {
    Box::new(WithContext {
        context: context.into(),
        source: source.into(),
    })
}

pub type Result<T> = std::result::Result<T, ClusterAuthError>;

/// Walks `error` and everything beneath it.
///
/// [`std::error::Error::source`] alone is not enough here. `io::Error` does not expose the error
/// it wraps as its source - its `source` returns that error's *own* source - so anything a
/// transport buries in an `io::Error`, a TLS failure most of all, is invisible to a plain
/// `source()` loop. The `get_ref` hop puts it back in the chain.
///
/// Used to classify failures by matching the error that caused them rather than by searching
/// rendered text, which stops at the outermost message and matches on coincidence.
pub fn chain<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> impl Iterator<Item = &'a (dyn std::error::Error + 'static)> {
    std::iter::successors(Some(error), |error| {
        error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .map(|inner| inner as &(dyn std::error::Error + 'static))
            .or_else(|| error.source())
    })
}

#[derive(Debug, Error)]
pub enum ClusterAuthError {
    #[error("Failed to connect to remote cluster {cluster}")]
    RemoteClusterConnection { cluster: String, source: BoxError },

    #[error("Configuration error")]
    ConfigError(#[source] BoxError),

    #[error("Internal error")]
    Internal(#[source] BoxError),
}

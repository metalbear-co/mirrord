use std::{error::Error, sync::Arc};

use thiserror::Error;

/// Errors that can occur in the [`RedirectorTask`](super::RedirectorTask).
#[derive(Error, Debug, Clone)]
pub enum RedirectorTaskError {
    /// A runtime error that occurred in the task.
    ///
    /// For simplicity, the inner error is opaque.
    /// Any redirector task error should terminate the agent,
    /// so we're not really interested in the concrete type.
    #[error("port redirector failed: {0}")]
    RedirectorError(Arc<dyn Error + Send + Sync + 'static>),
    /// The task panicked.
    #[error("port redirector task panicked")]
    Panicked,
}

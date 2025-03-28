use std::{error::Error, sync::Arc};

use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum RedirectorTaskError {
    #[error("port redirector failed: {0}")]
    RedirectorError(Arc<dyn Error + Send + Sync + 'static>),
    #[error("port redirector task panicked")]
    Panicked,
}

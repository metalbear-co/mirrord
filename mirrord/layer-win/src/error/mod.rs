//! `layer-win` errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {}

pub type Result<T> = std::result::Result<T, Error>;

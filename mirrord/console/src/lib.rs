#![warn(clippy::indexing_slicing)]

pub mod error;
pub mod logger;
pub mod protocol;

pub use logger::init_logger;

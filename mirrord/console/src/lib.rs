#![warn(clippy::indexing_slicing)]

pub mod async_logger;
pub mod error;
pub mod logger;
pub mod protocol;

pub use async_logger::init_async_logger;
pub use logger::init_logger;

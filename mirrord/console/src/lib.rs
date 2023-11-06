#![warn(clippy::indexing_slicing)]

#[cfg(feature = "async-logger")]
pub mod async_logger;
pub mod error;
pub mod logger;
pub mod protocol;

#[cfg(feature = "async-logger")]
pub use async_logger::init_async_logger;
pub use logger::init_logger;

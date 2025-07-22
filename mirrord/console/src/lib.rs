#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

/// Silences `deny(unused_crate_dependencies)`.
///
/// These dependencies are only used in the console binary.
#[cfg(feature = "binary")]
mod binary_deps {
    use tokio as _;
    use tracing as _;
    use tracing_subscriber as _;
}

#[cfg(feature = "async-logger")]
pub mod async_logger;
pub mod error;
pub mod logger;
pub mod protocol;

#[cfg(feature = "async-logger")]
pub use async_logger::init_async_logger;
pub use logger::init_logger;

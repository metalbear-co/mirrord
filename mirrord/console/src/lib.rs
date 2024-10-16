#![deny(unused_crate_dependencies)]
#![warn(clippy::indexing_slicing)]

#[cfg(feature = "async-logger")]
pub mod async_logger;
pub mod error;
pub mod logger;
pub mod protocol;

#[cfg(feature = "async-logger")]
pub use async_logger::init_async_logger;
pub use logger::init_logger;

#[cfg(feature = "binary")]
mod deps_used_in_binary {
    //! To silence false positive from `unused_crate_dependencies`.
    //!
    //! See [discussion on GitHub](https://github.com/rust-lang/cargo/issues/12717#issuecomment-1728123462) for reference.

    use tracing as _;
    use tracing_subscriber as _;
}

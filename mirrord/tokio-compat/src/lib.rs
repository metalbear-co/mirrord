//! New [`hyper`] version uses its own IO and executor traits. This crate provides a thin
//! compatibility layer over [`tokio`] types.
//!
//! ## TODO
//! This crate should be removed once [hyper-util](https://github.com/hyperium/hyper-util) is released.

mod executor;
mod io;

pub use executor::TokioExecutor;
pub use io::{TokioIo, WrapIo};

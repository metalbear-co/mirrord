//! Module responsible for handling Windows only errors.
//!
//! `WindowsError` itself now lives in the `utils-win` crate. It is re-exported here.
//! Existing call sites (`crate::error::windows::WindowsError`) keep working unchanged.

pub use mirrord_utils_win::error::{WindowsError, WindowsResult};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConsoleError {
    #[error("Failed to allocate console: {0}")]
    FailedAllocatingConsole(WindowsError),

    #[error("Failed to redirect standard handles: {0}")]
    FailedRedirectingStdHandles(WindowsError),
}
pub type ConsoleResult<T> = Result<T, ConsoleError>;

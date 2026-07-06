//! Module responsible for handling Windows only errors.
//!
//! `WindowsError` itself now lives in the `utils-win` crate. It is re-exported here.
//! Existing call sites (`crate::error::windows::WindowsError`) keep working unchanged.

use thiserror::Error;
pub use utils_win::error::{WindowsError, WindowsResult};

#[derive(Error, Debug)]
pub enum ConsoleError {
    #[error("Failed to allocate console: {0}")]
    FailedAllocatingConsole(WindowsError),

    #[error("Failed to redirect standard handles: {0}")]
    FailedRedirectingStdHandles(WindowsError),
}
pub type ConsoleResult<T> = Result<T, ConsoleError>;

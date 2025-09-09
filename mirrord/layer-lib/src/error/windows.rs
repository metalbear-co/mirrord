//! Module responsible for handling Windows errors.
#![cfg(target_os = "windows")]

use std::fmt::{Debug, Display};

use str_win::u16_buffer_to_string;
use thiserror::Error;
use winapi::{
    shared::ntdef::{MAKELANGID, SUBLANG_ENGLISH_US},
    um::{
        errhandlingapi::GetLastError,
        winbase::{FORMAT_MESSAGE_FROM_SYSTEM, FormatMessageW},
        winnt::LANG_ENGLISH,
    },
};

#[derive(Error, Debug)]
pub enum ConsoleError {
    #[error("Failed to allocate console: {0}")]
    FailedAllocatingConsole(WindowsError),

    #[error("Failed to redirect standard handles: {0}")]
    FailedRedirectingStdHandles(WindowsError),
}
pub type ConsoleResult<T> = Result<T, ConsoleError>;

pub struct WindowsError {
    /// Usually returned by [`winapi::umm::errhandlingapi::GetLastError`].
    error: u32,
}

impl WindowsError {
    /// Generate new [`WindowsError`] from Windows system error code.
    ///
    /// # Arguments
    ///
    /// * `error` - Windows system error code.
    pub fn new(error: u32) -> Self {
        Self { error }
    }

    /// Generate a new [`WindowsError`] from [`GetLastError`].
    pub fn last_error() -> Self {
        let error = unsafe { GetLastError() };
        Self { error }
    }

    /// Get raw Windows system error code.
    pub fn get_error(&self) -> u32 {
        self.error
    }

    /// Returns an en-US string of a Windows system error code.
    ///
    /// # Arguments
    ///
    /// * `error` - Windows system error code.
    pub fn format_windows_error_code(error: u32) -> Option<String> {
        let mut buf: [u16; 256] = [0; 256];

        // Use MAKELANGID macro to get en-US language ID.
        let english_us = MAKELANGID(LANG_ENGLISH, SUBLANG_ENGLISH_US);

        // [`FormatMessageW`] returns the number of `TCHAR`'s returned by the function.
        // If succesful, number is non-zero.
        // Also, the [`FormatMessageW`] definition is also pretty stupid, so we have to do some
        // casting magic.
        let ret = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM,
                std::ptr::null(),
                error,
                english_us as _,
                // Make sure we take `buf` as mutable here and that we don't cast the
                // constness away.
                buf.as_mut_ptr() as _,
                256,
                std::ptr::null_mut(),
            )
        };
        if ret == 0 {
            return None;
        }

        // [`trim_ascii`] gets rid of carriage return and endline.
        Some(u16_buffer_to_string(buf).trim_ascii().to_string())
    }

    /// Returns an en-US string of our Windows system error code.
    pub fn get_formatted_error(&self) -> Option<String> {
        Self::format_windows_error_code(self.error)
    }
}

impl From<u32> for WindowsError {
    fn from(val: u32) -> Self {
        WindowsError::new(val)
    }
}

impl Display for WindowsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{}",
            self.get_formatted_error()
                .unwrap_or("Not a valid Windows error code".into())
        ))
    }
}

impl Debug for WindowsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{}",
            self.get_formatted_error()
                .unwrap_or("Not a valid Windows error code".into())
        ))
    }
}

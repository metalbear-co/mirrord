//! Module responsible for handling Windows only errors.

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

#[derive(Error)]
pub enum WindowsError {
    /// Usually returned by [`winapi::umm::errhandlingapi::GetLastError`].
    Windows(u32),
    /// Usually returned by Winsock functions. [`winapi::um::winsock2::WSAGetLastError`].
    WinSock(i32),
}
pub type WindowsResult<T, E = WindowsError> = Result<T, E>;

impl WindowsError {
    /// Generate a new [`WindowsError`] from [`GetLastError`].
    pub fn last_error() -> Self {
        let error = unsafe { GetLastError() };
        Self::Windows(error)
    }

    /// Generate a new [`WindowsError`] from [`WSAGetLastError`].
    pub fn wsa_last_error() -> Self {
        let error = unsafe { winapi::um::winsock2::WSAGetLastError() };
        Self::WinSock(error)
    }

    /// Returns an en-US string of a Windows system error code.
    ///
    /// # Arguments
    ///
    /// * `error` - Windows system error code.
    pub fn format_error_code(error: u32) -> Option<String> {
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
        let err: u32 = match self {
            Self::Windows(code) => *code,
            Self::WinSock(code) => *code as _,
        };
        Self::format_error_code(err)
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

// Allow direct comparison between WindowsError and i32 for WinSock errors
impl PartialEq<i32> for WindowsError {
    fn eq(&self, other: &i32) -> bool {
        match self {
            Self::Windows(_) => false,
            Self::WinSock(code) => code == other,
        }
    }
}

// Allow direct comparison between WindowsError and u32 for Windows errors
impl PartialEq<u32> for WindowsError {
    fn eq(&self, other: &u32) -> bool {
        match self {
            Self::Windows(code) => code == other,
            Self::WinSock(_) => false,
        }
    }
}

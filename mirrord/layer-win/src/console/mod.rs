//! Module responsible for handling the Windows console for debugging.

use mirrord_layer_lib::error::windows::{ConsoleError, ConsoleResult, WindowsError};
use winapi::um::{
    consoleapi::AllocConsole,
    fileapi::{CreateFileA, OPEN_EXISTING},
    handleapi::INVALID_HANDLE_VALUE,
    processenv::SetStdHandle,
    winbase::{STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    winnt::{
        FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE,
    },
};

/// Creates a new Windows console, for debugging purposes. Also responsible
/// for redirecting `stdin`, `stdout` and `stderr`, to `CONIN$` and `CONOUT$`.
pub fn create() -> ConsoleResult<()> {
    let ret = unsafe { AllocConsole() };
    if ret == 0 {
        return Err(ConsoleError::FailedAllocatingConsole(
            WindowsError::last_error(),
        ));
    }

    // Make stdout/stderr/stdin writes actually appear on console.
    redirect_std_handles()?;

    Ok(())
}

fn redirect_std_handles() -> ConsoleResult<()> {
    unsafe {
        let out_handle = CreateFileA(
            c"CONOUT$".as_ptr() as _,
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_WRITE | FILE_SHARE_READ,
            std::ptr::null_mut() as _,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut() as _,
        );

        if out_handle != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, out_handle);
            SetStdHandle(STD_ERROR_HANDLE, out_handle);
        } else {
            return Err(ConsoleError::FailedRedirectingStdHandles(
                WindowsError::last_error(),
            ));
        }

        // Open CONIN$ (console input)
        let in_handle = CreateFileA(
            c"CONIN$".as_ptr() as _,
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_WRITE | FILE_SHARE_READ,
            std::ptr::null_mut() as _,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut() as _,
        );

        if in_handle != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_INPUT_HANDLE, in_handle);
        } else {
            return Err(ConsoleError::FailedRedirectingStdHandles(
                WindowsError::last_error(),
            ));
        }
    }

    Ok(())
}

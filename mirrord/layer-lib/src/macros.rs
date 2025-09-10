//! Cross-platform macros for layer-lib.

/// Cross-platform graceful exit macro.
///
/// Exits the current process gracefully by sending a SIGKILL signal on Unix platforms
/// or calling TerminateProcess on Windows.
///
/// ## Examples
///
/// ```rust, no_run
/// graceful_exit!("mirrord encountered a critical error: {}", error_message);
/// ```
#[cfg(not(target_os = "windows"))]
#[macro_export]
macro_rules! graceful_exit {
    ($($arg:tt)+) => {{
        eprintln!($($arg)+);
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(std::process::id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        )
        .expect("unable to graceful exit");
        unreachable!()
    }};
    () => {{
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(std::process::id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        )
        .expect("unable to graceful exit");
        unreachable!()
    }};
}

#[cfg(target_os = "windows")]
#[macro_export]
macro_rules! graceful_exit {
    ($($arg:tt)+) => {{
        eprintln!($($arg)+);
        unsafe {
            winapi::um::processthreadsapi::TerminateProcess(
                winapi::um::processthreadsapi::GetCurrentProcess(),
                1
            );
        }
        unreachable!()
    }};
    () => {{
        unsafe {
            winapi::um::processthreadsapi::TerminateProcess(
                winapi::um::processthreadsapi::GetCurrentProcess(),
                1
            );
        }
        unreachable!()
    }};
}

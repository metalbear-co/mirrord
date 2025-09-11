//! Cross-platform macros for layer-lib.
//! Macros used by mirrord-layer-lib
//!
//! ## Macros
//!
//! - [`graceful_exit!`](`macro@crate::graceful_exit`)
//!
//! Exits the process with a nice message.

/// Kills the process and prints a helpful error message to the user.
///
/// ## Parameters
///
/// - `$arg`: messages to print, supports [`println!`] style arguments.
///
/// ## Examples
///
/// - Exiting on IO failure:
///
/// ```rust, no_run
/// if let Err(fail) = File::open("nothing.txt") {
///     graceful_exit!("mirrord failed to open file with {:#?}", fail);
/// }
/// ```
#[cfg(not(target_os = "windows"))]
#[macro_export]
macro_rules! graceful_exit {
    ($($arg:tt)+) => {{
        eprintln!($($arg)+);
        graceful_exit!()
    }};
    () => {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(std::process::id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        )
        .expect("unable to graceful exit");
    };
}

#[cfg(target_os = "windows")]
#[macro_export]
macro_rules! graceful_exit {
    ($($arg:tt)+) => {{
        eprintln!($($arg)+);
        graceful_exit!()
    }};
    () => {{
        unsafe {
            use winapi::um::processthreadsapi::{GetCurrentProcess, TerminateProcess};
            let _ = TerminateProcess(GetCurrentProcess(), 1);
        }
        unreachable!()
    }};
}

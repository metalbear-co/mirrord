//! The environment `mirrord exec` launches the user's process with.
//!
//! Environment variable names are case-sensitive on unix and case-insensitive on Windows, where
//! `Path` and `PATH` are one variable. [`ProcessEnv`] matches names the way the platform does, so
//! `compose_exec_environment` in `main.rs` is the same code on both.

#[cfg(not(windows))]
use std::collections::HashMap;

#[cfg(windows)]
use mirrord_layer_lib::process::windows::environment::WindowsEnv;

/// A child process environment, keyed by variable name.
#[cfg(windows)]
pub(crate) type ProcessEnv = WindowsEnv;

/// A child process environment, keyed by variable name.
#[cfg(not(windows))]
pub(crate) type ProcessEnv = HashMap<String, String>;

/// This process's environment, one entry per variable.
pub(crate) fn inherited() -> ProcessEnv {
    #[cfg(windows)]
    {
        WindowsEnv::inherited()
    }

    #[cfg(not(windows))]
    {
        std::env::vars().collect()
    }
}

/// Sets `name` to `value`, replacing the variable under any spelling the platform treats as the
/// same name.
pub(crate) fn set(env: &mut ProcessEnv, name: &str, value: String) {
    #[cfg(windows)]
    {
        env.set(name, value);
    }

    #[cfg(not(windows))]
    {
        env.insert(name.to_owned(), value);
    }
}


use base64::prelude::*;
use libc::{c_char, c_int};
#[cfg(not(target_os = "macos"))]
use mirrord_layer_macro::hook_fn;
#[cfg(target_os = "macos")]
use mirrord_layer_macro::hook_guard_fn;

use super::*;
#[cfg(not(target_os = "macos"))]
use crate::common::CheckedInto;
#[cfg(target_os = "macos")]
use crate::exec_utils::*;
use crate::{
    SOCKETS,
    common::proxy_conn_fd,
    detour::{Bypass, Detour},
    hooks::HookManager,
    proxy_connection::FD_ENV_VAR,
    replace,
    socket::SHARED_SOCKETS_ENV_VAR,
};

/// Takes an [`Argv`] with the enviroment variables from an `exec` call, extending it with
/// an encoded version of our [`SOCKETS`] and [`FD_ENV_VAR`].
///
/// The check for [`libc::FD_CLOEXEC`] is performed during the [`SOCKETS`] initialization
/// by the child process.
pub(crate) fn prepare_execve_envp(env_vars: Detour<Argv>) -> Detour<Argv> {
    let mut env_vars = env_vars.or_bypass(|reason| match reason {
        Bypass::EmptyOption => Detour::Success(Argv(Vec::new())),
        other => Detour::Bypass(other),
    })?;

    let lock = SOCKETS.lock()?;
    let shared_sockets = lock
        .iter()
        .map(|(key, value)| (*key, value))
        .collect::<Vec<_>>();

    let encoded = bincode::encode_to_vec(shared_sockets, bincode::config::standard())
        .map(|bytes| BASE64_URL_SAFE.encode(bytes))?;

    drop(lock);

    env_vars.insert_env(SHARED_SOCKETS_ENV_VAR, &encoded)?;
    env_vars.insert_env(FD_ENV_VAR, &proxy_conn_fd().unwrap().to_string())?;

    Detour::Success(env_vars)
}

#[cfg(not(target_os = "macos"))]
unsafe fn environ() -> *const *const c_char {
    unsafe {
        unsafe extern "C" {
            static environ: *const *const c_char;
        }

        environ
    }
}

/// Hook for `libc::execv` for linux only.
///
/// On macos this just calls `execve(path, argv, _environ)`, so we'll be handling it in our
/// [`execve_detour`].
#[cfg(not(target_os = "macos"))]
#[hook_fn]
unsafe extern "C" fn execv_detour(path: *const c_char, argv: *const *const c_char) -> c_int {
    unsafe {
        let envp = environ();
        match prepare_execve_envp(envp.checked_into()) {
            Detour::Success(envp) => FN_EXECVE(path, argv, envp.leak()),
            _ => FN_EXECVE(path, argv, envp),
        }
    }
}

/// Hook for `libc::execve`.
///
/// We can't change the pointers, to get around that we create our own and **leak** them.
#[cfg(not(target_os = "macos"))]
#[hook_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    unsafe {
        match prepare_execve_envp(envp.checked_into()) {
            Detour::Success(envp) => FN_EXECVE(path, argv, envp.leak()),
            _ => FN_EXECVE(path, argv, envp),
        }
    }
}

/// Hook for `libc::execve`.
///
/// We can't change the pointers, to get around that we create our own and **leak** them.
///
/// - #[cfg(target_os = "macos")]
///
/// We change 3 arguments and then call the original functions:
///
/// 1. The executable path - we check it for SIP, create a patched binary and use the path to the
/// new path instead of the original path. If there is no SIP, we use a new string with the same
/// path.
/// 2. argv - we strip mirrord's temporary directory from the start of arguments.
/// So if `argv[1]` is "/var/folders/1337/mirrord-bin/opt/homebrew/bin/npx", switch it
/// to "/opt/homebrew/bin/npx". Also here we create a new array with pointers
/// to new strings, even if there are no changes needed (except for the case of an error).
/// 3. envp - We found out that Turbopack (Vercel) spawns a clean "Node" instance without env,
/// basically stripping all of the important mirrord env.
/// [#2500](https://github.com/metalbear-co/mirrord/issues/2500)
/// We restore the `DYLD_INSERT_LIBRARIES` environment variable and all env vars
/// starting with `MIRRORD_` if the dyld var can't be found in `envp`.
///
/// If there is an error in the detour, we don't exit or anything, we just call the original libc
/// function with the original passed arguments.
#[cfg(target_os = "macos")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    unsafe {
        match patch_sip_for_new_process(path, argv, envp) {
            Detour::Success((path, argv, envp)) => {
                match prepare_execve_envp(Detour::Success(envp.clone())) {
                    Detour::Success(envp) => {
                        FN_EXECVE(path.into_raw().cast_const(), argv.leak(), envp.leak())
                    }
                    _ => FN_EXECVE(path.into_raw().cast_const(), argv.leak(), envp.leak()),
                }
            }
            _ => FN_EXECVE(path, argv, envp),
        }
    }
}

/// Enables `exec` hooks.
pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    unsafe {
        #[cfg(not(target_os = "macos"))]
        replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);

        replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
    }
}

use std::{ffi::CString, ops::Not, os::unix::process::parent_id};

use base64::prelude::*;
use libc::{c_char, c_int, FD_CLOEXEC};
use mirrord_layer_macro::hook_guard_fn;
use tracing::Level;

use super::Argv;
#[cfg(target_os = "macos")]
use crate::exec_utils::*;
use crate::{
    detour::Detour,
    hooks::HookManager,
    replace,
    socket::{hooks::FN_FCNTL, UserSocket, SHARED_SOCKETS_ENV_VAR},
    SOCKETS,
};

#[tracing::instrument(level = Level::DEBUG, ret)]
fn shared_sockets() -> Vec<(i32, UserSocket)> {
    SOCKETS
        .iter()
        .filter_map(|inner| {
            let is_cloexec = unsafe { FN_FCNTL(*inner.key(), libc::F_GETFD) } & FD_CLOEXEC > 0;
            is_cloexec
                .not()
                .then(|| (*inner.key(), UserSocket::clone(inner.value())))
        })
        .collect::<Vec<_>>()
}

#[tracing::instrument(level = Level::DEBUG, ret, fields(pid = std::process::id(), parent_pid = parent_id()))]
fn execve() -> Detour<Argv> {
    let encoded = bincode::encode_to_vec(shared_sockets(), bincode::config::standard())
        .map(|bytes| BASE64_URL_SAFE.encode(bytes))
        .unwrap_or_default();

    let mut env_vars = std::env::vars()
        .filter_map(|(key, var)| CString::new(format!("{key}={var}")).ok())
        .collect::<Argv>();

    env_vars.push(CString::new(format!("{SHARED_SOCKETS_ENV_VAR}={encoded}"))?);

    Detour::Success(env_vars)
}

#[cfg(not(target_os = "macos"))]
#[hook_guard_fn]
unsafe extern "C" fn execv_detour(path: *const c_char, argv: *const *const c_char) -> c_int {
    tracing::info!("execv called");
    if let Detour::Success(env_vars) = execve() {
        FN_EXECVE(path, argv, env_vars.null_vec().as_ptr() as *const *const _)
    } else {
        FN_EXECV(path, argv)
    }
}

#[cfg(target_os = "macos")]
#[hook_guard_fn]
unsafe extern "C" fn execv_detour(path: *const c_char, argv: *const *const c_char) -> c_int {
    tracing::info!("execv called");
    if let Detour::Success(env_vars) = execve() {
        let envp = env_vars.null_vec().as_ptr() as *const *const _;

        if let Detour::Success((new_path, new_argv, new_envp)) =
            patch_sip_for_new_process(path, argv, envp)
        {
            let new_argv = new_argv.null_vec();
            let new_envp = new_envp.null_vec();
            FN_EXECVE(
                new_path.as_ptr(),
                new_argv.as_ptr() as *const *const c_char,
                new_envp.as_ptr() as *const *const c_char,
            )
        } else {
            FN_EXECV(path, argv)
        }
    } else {
        FN_EXECV(path, argv)
    }
}

#[cfg(not(target_os = "macos"))]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    tracing::info!("execve called");
    if let Detour::Success(env_vars) = execve() {
        FN_EXECVE(path, argv, env_vars.null_vec().as_ptr() as *const *const _)
    } else {
        FN_EXECVE(path, argv, envp)
    }
}

#[cfg(target_os = "macos")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    tracing::info!("execve called");
    if let Detour::Success(env_vars) = execve() {
        let envp = env_vars.null_vec().as_ptr() as *const *const _;

        if let Detour::Success((new_path, new_argv, new_envp)) =
            patch_sip_for_new_process(path, argv, envp)
        {
            let new_argv = new_argv.null_vec();
            let new_envp = new_envp.null_vec();
            FN_EXECVE(
                new_path.as_ptr(),
                new_argv.as_ptr() as *const *const c_char,
                new_envp.as_ptr() as *const *const c_char,
            )
        } else {
            FN_EXECVE(path, argv, envp)
        }
    } else {
        FN_EXECVE(path, argv, envp)
    }
}

pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);
    replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
}

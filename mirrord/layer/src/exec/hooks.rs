use std::{
    ffi::{CStr, CString},
    os::unix::process::parent_id,
};

use base64::prelude::*;
use libc::{c_char, c_int, FD_CLOEXEC};
use mirrord_layer_macro::hook_guard_fn;
use null_terminated::Nul;
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
            // let is_shared = unsafe { FN_FCNTL(*inner.key(), libc::F_GETFD) } & FD_CLOEXEC > 0;
            let is_shared = true;
            is_shared.then(|| (*inner.key(), UserSocket::clone(inner.value())))
        })
        .collect::<Vec<_>>()
}

#[mirrord_layer_macro::instrument(
    level = Level::DEBUG,
    ret,
    fields(
        pid = std::process::id(),
        parent_pid = parent_id()
    )
)]
fn execve(env_vars: &mut Argv) -> Detour<()> {
    let encoded = bincode::encode_to_vec(shared_sockets(), bincode::config::standard())
        .map(|bytes| BASE64_URL_SAFE.encode(bytes))
        .unwrap_or_default();

    env_vars.push(CString::new(format!("{SHARED_SOCKETS_ENV_VAR}={encoded}"))?);

    Detour::Success(())
}

#[hook_guard_fn]
unsafe extern "C" fn execv_detour(path: *const c_char, argv: *const *const c_char) -> c_int {
    let mut env_vars = std::env::vars()
        .filter_map(|(key, var)| CString::new(format!("{key}={var}")).ok())
        .collect::<Argv>();

    if let Detour::Success(()) = execve(&mut env_vars) {
        FN_EXECVE(path, argv, env_vars.null_vec().as_ptr() as *const *const _)
    } else {
        FN_EXECV(path, argv)
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    let rawish_envp = Nul::new_unchecked(envp);
    let mut env_vars = rawish_envp
        .iter()
        .map(|raw_env_var| unsafe { CStr::from_ptr(*raw_env_var) }.to_owned())
        .collect::<Argv>();

    if let Detour::Success(()) = execve(&mut env_vars) {
        FN_EXECVE(path, argv, env_vars.null_vec().as_ptr() as *const *const _)
    } else {
        FN_EXECVE(path, argv, envp)
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fexecve_detour(
    fd: c_int,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    let rawish_envp = Nul::new_unchecked(envp);
    let mut env_vars = rawish_envp
        .iter()
        .map(|raw_env_var| unsafe { CStr::from_ptr(*raw_env_var) }.to_owned())
        .collect::<Argv>();

    if let Detour::Success(()) = execve(&mut env_vars) {
        FN_FEXECVE(fd, argv, env_vars.null_vec().as_ptr() as *const *const _)
    } else {
        FN_FEXECVE(fd, argv, envp)
    }
}

pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);
    replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
    replace!(
        hook_manager,
        "fexecve",
        fexecve_detour,
        FnFexecve,
        FN_FEXECVE
    );
}

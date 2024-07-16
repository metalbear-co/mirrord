use std::{ffi::CString, ops::Not, os::unix::process::parent_id};

use base64::prelude::*;
use libc::{c_char, c_int, FD_CLOEXEC};
use mirrord_layer_macro::hook_guard_fn;
use tracing::Level;

use super::Argv;
use crate::{
    detour::Detour,
    hooks::HookManager,
    replace,
    socket::{hooks::FN_FCNTL, UserSocket},
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

    env_vars.push(CString::new(format!("{SHARED}={encoded}"))?);

    Detour::Success(env_vars)
}

#[hook_guard_fn]
unsafe extern "C" fn execv_detour(prog: *const c_char, argv: *const *const c_char) -> c_int {
    tracing::info!("execv called");
    if let Detour::Success(env_vars) = execve() {
        FN_EXECVE(prog, argv, env_vars.null_vec().as_ptr() as *const *const _)
    } else {
        FN_EXECV(prog, argv)
    }
}

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

const SHARED: &str = "MIRRORD_SHARED_SOCKETS";

pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);
    replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
}

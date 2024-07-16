use std::{
    borrow::BorrowMut,
    ffi::{CStr, CString},
    ops::Not,
};

use base64::prelude::*;
use libc::{c_char, c_int, FD_CLOEXEC};
use mirrord_intproxy_protocol::{net::UserSocket as ProxyUserSocket, ExecveRequest};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use null_terminated::Nul;
use tracing::Level;

use super::Argv;
use crate::{
    common,
    detour::Detour,
    hooks::HookManager,
    replace,
    socket::{hooks::FN_FCNTL, UserSocket},
    SOCKETS,
};

#[hook_guard_fn]
unsafe extern "C" fn execl_detour(
    path: *const c_char,
    arg0: *const c_char,
    mut args: ...
) -> c_int {
    tracing::info!("execl");

    FN_EXECL(path, arg0, args)
}

#[hook_guard_fn]
unsafe extern "C" fn execlp_detour(
    file: *const c_char,
    arg0: *const c_char,
    mut args: ...
) -> c_int {
    tracing::info!("execlp");

    FN_EXECLP(file, arg0, args)
}

#[hook_guard_fn]
unsafe extern "C" fn execle_detour(
    path: *const c_char,
    arg0: *const c_char,
    mut args: ...
) -> c_int {
    tracing::info!("execle");

    FN_EXECLE(path, arg0, args)
}

#[hook_guard_fn]
#[tracing::instrument(level = Level::INFO, ret)]
unsafe extern "C" fn execv_detour(prog: *const c_char, argv: *const *const c_char) -> c_int {
    FN_EXECV(prog, argv)
}

#[hook_guard_fn]
#[tracing::instrument(level = Level::INFO, ret)]
unsafe extern "C" fn execvp_detour(c: *const c_char, argv: *const *const c_char) -> c_int {
    FN_EXECVP(c, argv)
}

#[hook_guard_fn]
#[tracing::instrument(level = Level::INFO, ret)]
unsafe extern "C" fn execvpe_detour(
    file: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    FN_EXECVPE(file, argv, envp)
}

#[hook_guard_fn]
// #[tracing::instrument(level = Level::INFO, ret)]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    match execve(Nul::new_unchecked(envp)) {
        Detour::Success(new_envp) => {
            FN_EXECVE(path, argv, new_envp.null_vec().as_ptr() as *const *const _)
        }
        _ => FN_EXECVE(path, argv, envp),
    }
}

// TODO(alex) [high]: Set env var and save sockets.
#[mirrord_layer_macro::instrument(level = Level::INFO, skip(rawish_envp), ret)]
pub(super) fn execve(rawish_envp: &Nul<*const c_char>) -> Detour<Argv> {
    let shared_sockets = SOCKETS
        .iter()
        .filter_map(|inner| {
            let cloexec = unsafe { FN_FCNTL(*inner.key(), libc::F_GETFD) };
            tracing::info!(
                "socket has flag {cloexec:?} sock {:?} {:?} {:?}",
                inner.key(),
                inner.value(),
                errno::errno()
            );
            if cloexec == FD_CLOEXEC {
                None
            } else {
                let cloned = UserSocket::clone(inner.value());
                Some((*inner.key(), cloned))
            }
        })
        .collect::<Vec<_>>();

    let encoded = bincode::encode_to_vec(shared_sockets, bincode::config::standard())
        .map(|bytes| BASE64_URL_SAFE.encode(bytes))?;

    let mut env_vars = rawish_envp
        .iter()
        .map(|raw_env_var| unsafe { CStr::from_ptr(*raw_env_var) }.to_owned())
        .collect::<Argv>();

    env_vars.push(CString::new(format!("MIRRORD_SHARED_SOCKETS={}", encoded))?);

    Detour::Success(env_vars)
}

pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    // replace!(hook_manager, "execl", execl_detour, FnExecl, FN_EXECL);
    // replace!(hook_manager, "execlp", execlp_detour, FnExeclp, FN_EXECLP);
    // replace!(hook_manager, "execle", execle_detour, FnExecle, FN_EXECLE);
    // replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);
    // replace!(hook_manager, "execvp", execvp_detour, FnExecvp, FN_EXECVP);
    // replace!(
    //     hook_manager,
    //     "execvpe",
    //     execvpe_detour,
    //     FnExecvpe,
    //     FN_EXECVPE
    // );
    replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
    // replace!(hook_manager, "exec", exec_detour, FnExec, FN_EXEC);
}

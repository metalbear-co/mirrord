use libc::{c_char, c_int};
use mirrord_intproxy_protocol::{net::UserSocket as ProxyUserSocket, ExecveRequest};
use mirrord_layer_macro::hook_guard_fn;
use tracing::Level;

use crate::{common, detour::Detour, hooks::HookManager, replace, socket::UserSocket, SOCKETS};

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
#[tracing::instrument(level = Level::INFO, ret)]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    execve();
    FN_EXECVE(path, argv, envp)
}

// TODO(alex) [high]: Set env var and save sockets.
#[mirrord_layer_macro::instrument(level = Level::INFO, ret)]
pub(super) fn execve() -> Detour<()> {
    // if std::env::var("MIRRORD_PARENT")
    //     .inspect(|_| tracing::info!("Lets gooo"))
    //     .inspect_err(|fail| tracing::error!("Lets not gooo {fail:?}"))
    //     .is_ok()
    // {
    let shared_sockets = SOCKETS
        .iter()
        .filter_map(|inner| {
            // if FD_CLOEXEC & unsafe { FN_FCNTL(*inner.key(), 0) } > 0 {
            // None
            // } else {
            let cloned = UserSocket::clone(inner.value());
            Some((*inner.key(), cloned.into()))
            // }
        })
        .collect();
    tracing::info!("shared sockets {shared_sockets:?}");

    common::make_proxy_request_no_response(ExecveRequest { shared_sockets })?;
    // }

    Detour::Success(())
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

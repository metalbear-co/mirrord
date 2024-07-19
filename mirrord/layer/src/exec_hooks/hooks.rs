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

#[tracing::instrument(level = Level::TRACE)]
fn shared_sockets() -> Vec<(i32, UserSocket)> {
    SOCKETS
        .iter()
        .filter_map(|inner| {
            // We only want to share the sockets that are not `FD_CLOEXEC`.
            let is_shared = unsafe { FN_FCNTL(*inner.key(), libc::F_GETFD) } & FD_CLOEXEC > 0;
            is_shared.then(|| (*inner.key(), UserSocket::clone(inner.value())))
        })
        .collect::<Vec<_>>()
}

#[mirrord_layer_macro::instrument(
    level = Level::DEBUG,
    ret,
    skip(env_vars),
    fields(
        pid = std::process::id(),
        parent_pid = parent_id(),
        env_len = env_vars.len()
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
    let encoded = bincode::encode_to_vec(shared_sockets(), bincode::config::standard())
        .map(|bytes| BASE64_URL_SAFE.encode(bytes))
        .unwrap_or_default();

    std::env::set_var("MIRRORD_SHARED_SOCKETS", encoded);

    FN_EXECV(path, argv)
}

/// Hook for `libc::execve`.
///
/// - #[cfg(target_os = "macos")]
///
/// We change 3 arguments and then call the original functions:
/// 1. The executable path - we check it for SIP, create a patched binary and use the path to the
///    new path instead of the original path. If there is no SIP, we use a new string with the same
///    path.
/// 2. argv - we strip mirrord's temporary directory from the start of arguments. So if argv[1] is
///    "/var/folders/1337/mirrord-bin/opt/homebrew/bin/npx" Switch it to "/opt/homebrew/bin/npx"
///    Also here we create a new array with pointers to new strings, even if there are no changes
///    needed (except for the case of an error).
/// 3. envp - We found out that Turbopack (Vercel) spawns a clean "Node" instance without env,
///    basically stripping all of the important mirrord env.
///    https://github.com/metalbear-co/mirrord/issues/2500
///    We restore the `DYLD_INSERT_LIBRARIES` environment variable and all env vars starting with `MIRRORD_` if the dyld var can't be found in `envp`.
/// If there is an error in the detour, we don't exit or anything, we just call the original libc
/// function with the original passed arguments.
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
        let modified_envp = env_vars.null_vec().as_ptr() as *const *const c_char;

        #[cfg(target_os = "macos")]
        match patch_sip_for_new_process(path, argv, modified_envp) {
            Detour::Success((new_path, new_argv, new_envp)) => {
                let new_argv = new_argv.null_vec();
                let new_envp = new_envp.null_vec();
                FN_EXECVE(
                    new_path.as_ptr(),
                    new_argv.as_ptr() as *const *const c_char,
                    new_envp.as_ptr() as *const *const c_char,
                )
            }
            _ => FN_EXECVE(path, argv, envp),
        }

        tracing::info!("Success execve!");
        #[cfg(target_os = "linux")]
        FN_EXECVE(path, argv, modified_envp)
    } else {
        tracing::info!("Not success execve!");
        FN_EXECVE(path, argv, envp)
    }
}

pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);
    replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
}

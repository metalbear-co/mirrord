#[cfg(not(target_os = "macos"))]
use std::sync::atomic::{AtomicBool, Ordering};

use base64::prelude::*;
use libc::{c_char, c_int};
#[cfg(not(target_os = "macos"))]
use mirrord_layer_core::{envp::Envp, hooks::ProbedCall};
use mirrord_layer_lib::detour::{Bypass, Detour};
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
    hooks::HookManager,
    replace,
    socket::{SHARED_SOCKETS_ENV_VAR, SOCKETS, UserSocket},
};

#[cfg(not(target_os = "macos"))]
static EXECVE_PROBE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Converts the [`SOCKETS`] map into a vector of pairs `(Fd, UserSocket)`, so we can rebuild
/// it as a map.
fn shared_sockets() -> Detour<Vec<(i32, UserSocket)>> {
    Detour::Success(
        SOCKETS
            .lock()?
            .iter()
            .map(|(key, value)| (*key, value.as_ref().clone()))
            .collect::<Vec<_>>(),
    )
}

/// Takes an [`Argv`] with the enviroment variables from an `exec` call, extending it with
/// an encoded version of our [`SOCKETS`].
///
/// The check for [`libc::FD_CLOEXEC`] is performed during the [`SOCKETS`] initialization
/// by the child process.
pub(crate) fn prepare_execve_envp(env_vars: Detour<Argv>) -> Detour<Argv> {
    let mut env_vars = env_vars.or_bypass(|reason| match reason {
        Bypass::EmptyOption => Detour::Success(Argv(Vec::new())),
        other => Detour::Bypass(other),
    })?;

    env_vars.insert_env(SHARED_SOCKETS_ENV_VAR, &encoded_shared_sockets()?)?;

    Detour::Success(env_vars)
}

/// Encodes [`SOCKETS`] as the value of [`SHARED_SOCKETS_ENV_VAR`], which the layer in the new
/// image reads to rebuild them.
fn encoded_shared_sockets() -> Detour<String> {
    let encoded = bincode::encode_to_vec(shared_sockets()?, bincode::config::standard())
        .map(|bytes| BASE64_URL_SAFE.encode(bytes))?;

    Detour::Success(encoded)
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

/// Hook for `libc::execv` on Linux.
///
/// `execv` reaches the `execve` probe when it is installed. When installation fails, this detour
/// retains the previous prepared-environment path so `execv` children still receive socket
/// metadata.
#[cfg(not(target_os = "macos"))]
#[hook_fn]
unsafe extern "C" fn execv_detour(path: *const c_char, argv: *const *const c_char) -> c_int {
    unsafe {
        let envp = environ();
        if EXECVE_PROBE_ENABLED.load(Ordering::Relaxed) {
            return libc::execve(path, argv, envp);
        }

        match prepare_execve_envp(envp.checked_into()) {
            Detour::Success(envp) => libc::execve(path, argv, envp.leak()),
            _ => libc::execve(path, argv, envp),
        }
    }
}

/// Replaces Linux `execve`'s environment with one that also carries socket metadata for the
/// new image.
///
/// The environment must stay valid until `execve` returns or replaces the image, and a probe
/// cannot free it after the call. In glibc’s `posix_spawn`, the `vfork` child shares the parent’s
/// memory, so the allocation stays in the parent on each spawn. To keep that leak small, [`Envp`]
/// reuses the strings in `envp`, which remain valid for the call, and allocates only the pointer
/// list and `MIRRORD_SHARED_SOCKETS` string.
#[cfg(not(target_os = "macos"))]
fn on_execve(call: &ProbedCall<'_>) {
    const ENVP: u32 = 2;

    let Detour::Success(encoded) = encoded_shared_sockets() else {
        return;
    };

    // SAFETY: `execve` requires `envp` to be null or a null-terminated array of C strings.
    let mut envp = unsafe { Envp::from_raw(call.arg(ENVP) as *const *const c_char) };
    envp.set(SHARED_SOCKETS_ENV_VAR, encoded.as_bytes());
    if let Some(prepared) = envp.into_raw() {
        call.set_arg(ENVP, prepared as usize);
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
    #[cfg(not(target_os = "macos"))]
    unsafe {
        replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);
    }

    #[cfg(not(target_os = "macos"))]
    if let Err(error) = hook_manager.probe_export_or_any("execve", on_execve) {
        tracing::warn!(
            ?error,
            "failed to install execve probe; new and spawned processes will not get shared socket metadata"
        );
    } else {
        EXECVE_PROBE_ENABLED.store(true, Ordering::Relaxed);
    }

    #[cfg(target_os = "macos")]
    unsafe {
        replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
    }
}

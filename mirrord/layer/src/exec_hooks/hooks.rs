use std::sync::OnceLock;

use base64::prelude::*;
use libc::{c_char, c_int};
use mirrord_config::debug_hold::dotnet_ports::{
    self, COMPLUS_DIAGNOSTIC_PORTS, DOTNET_DIAGNOSTIC_PORTS,
};
#[cfg(not(target_os = "macos"))]
use mirrord_layer_core::{envp::Envp, hooks::ProbedCall};
use mirrord_layer_lib::detour::{Bypass, Detour, DetourError, DetourExt};
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

/// Converts the [`SOCKETS`] map into a vector of pairs `(Fd, UserSocket)`, so we can rebuild
/// it as a map.
fn shared_sockets() -> Detour<Vec<(i32, UserSocket)>> {
    Ok(SOCKETS
        .lock()?
        .iter()
        .map(|(key, value)| (*key, value.as_ref().clone()))
        .collect::<Vec<_>>())
}

/// Takes an [`Argv`] with the enviroment variables from an `exec` call, extending it with
/// an encoded version of our [`SOCKETS`].
///
/// The check for [`libc::FD_CLOEXEC`] is performed during the [`SOCKETS`] initialization
/// by the child process.
pub(crate) fn prepare_execve_envp(env_vars: Detour<Argv>) -> Detour<Argv> {
    let mut env_vars = env_vars.or_bypass(|reason| match reason {
        Bypass::EmptyOption => Ok(Argv(Vec::new())),
        other => Err(DetourError::Bypass(other)),
    })?;

    env_vars.insert_env(SHARED_SOCKETS_ENV_VAR, &encoded_shared_sockets()?)?;
    if let Some(ports) = forwarded_dotnet_port(|key| env_vars.get_env(key)) {
        env_vars.insert_env(DOTNET_DIAGNOSTIC_PORTS, &ports)?;
    }

    Ok(env_vars)
}

/// This process's own `DOTNET_DiagnosticPorts` and `COMPlus_DiagnosticPorts`, read when the exec
/// hooks are enabled.
///
/// Read once, up front: the `execve` hook can run in the child of a `fork`, where a lock another
/// thread held at the fork is never released, so reading the environment there could wait
/// forever.
static DOTNET_PORTS: OnceLock<[Option<String>; 2]> = OnceLock::new();

/// The `DOTNET_DiagnosticPorts` a new image gets, so its runtimes pause on the port `mirrord up`
/// serves this process's (see [`dotnet_ports::forwarded`]). `None` leaves `envp` as it is.
///
/// # Arguments
///
/// * `child` - reads a variable of the `envp` the new image is started with.
fn forwarded_dotnet_port<'envp>(
    child: impl Fn(&'static str) -> Option<&'envp str>,
) -> Option<String> {
    let [dotnet, complus] = DOTNET_PORTS.get()?;
    dotnet_ports::forwarded(
        dotnet.as_deref(),
        complus.as_deref(),
        child(DOTNET_DIAGNOSTIC_PORTS),
        child(COMPLUS_DIAGNOSTIC_PORTS),
    )
}

/// Encodes [`SOCKETS`] as the value of [`SHARED_SOCKETS_ENV_VAR`], which the layer in the new
/// image reads to rebuild them.
fn encoded_shared_sockets() -> Detour<String> {
    let encoded = bincode::encode_to_vec(shared_sockets()?, bincode::config::standard())
        .map(|bytes| BASE64_URL_SAFE.encode(bytes))?;

    Ok(encoded)
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

/// Hook for `libc::execv` on Linux, installed only when the `execve` probe can't be.
///
/// With the probe installed, `execv` reaches it through `execve` and needs no hook. Without it,
/// this detour prepares the environment itself so `execv` children still receive socket metadata.
#[cfg(not(target_os = "macos"))]
#[hook_fn]
unsafe extern "C" fn execv_detour(path: *const c_char, argv: *const *const c_char) -> c_int {
    unsafe {
        let envp = environ();
        match prepare_execve_envp(envp.checked_into()) {
            Ok(envp) => libc::execve(path, argv, envp.leak()),
            _ => libc::execve(path, argv, envp),
        }
    }
}

/// Adds socket metadata for the new image to Linux `execve`'s environment, and the diagnostic port
/// `mirrord up` serves this process's runtimes when the caller's `envp` lacks it.
///
/// [`Envp`] reuses the caller's strings and allocates only the new entries and the pointer list,
/// which leak once per spawn. In glibc's `posix_spawn`, this runs on the small stack of the `vfork`
/// child (about 36 KiB), so keep large values off the stack.
#[cfg(not(target_os = "macos"))]
fn on_execve(call: &ProbedCall<'_>) {
    const ENVP: u32 = 2;

    // SAFETY: `execve` requires `envp` to be null or a null-terminated array of C strings.
    let mut envp = unsafe { Envp::from_raw(call.arg(ENVP) as *const *const c_char) };
    if let Ok(encoded) = encoded_shared_sockets() {
        envp.set(SHARED_SOCKETS_ENV_VAR, encoded.as_bytes());
    }
    let ports = forwarded_dotnet_port(|key| {
        envp.get(key)
            .and_then(|value| std::str::from_utf8(value).ok())
    });
    if let Some(ports) = ports {
        envp.set(DOTNET_DIAGNOSTIC_PORTS, ports.as_bytes());
    }
    if let Some(envp) = envp.into_raw() {
        call.set_arg(ENVP, envp as usize);
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
/// starting with `MIRRORD_` if the dyld var can't be found in `envp`, except the ones meant
/// for the first process alone (`MIRRORD_LAYER_READY`, `MIRRORD_LAYER_DEBUG_HOLD`), which the layer's
/// setup leaves out.
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
        let result = patch_sip_for_new_process(path, argv, envp);

        match result {
            Ok((path, argv, envp)) => match prepare_execve_envp(Ok(envp.clone())) {
                Ok(envp) => FN_EXECVE(path.into_raw().cast_const(), argv.leak(), envp.leak()),
                _ => FN_EXECVE(path.into_raw().cast_const(), argv.leak(), envp.leak()),
            },
            _ => FN_EXECVE(path, argv, envp),
        }
    }
}

/// Enables `exec` hooks.
pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    DOTNET_PORTS.get_or_init(|| {
        [DOTNET_DIAGNOSTIC_PORTS, COMPLUS_DIAGNOSTIC_PORTS].map(|name| std::env::var(name).ok())
    });

    // A replacement hook leaves frida's per-thread state behind when the function never returns,
    // as after a successful `exec` in a `vfork` child, so `execv` is replaced only as a fallback.
    #[cfg(not(target_os = "macos"))]
    if let Err(error) = hook_manager.probe_export_or_any("execve", on_execve) {
        tracing::warn!(
            ?error,
            "failed to install execve probe; processes started through execve or posix_spawn will not get shared socket metadata"
        );

        unsafe {
            replace!(hook_manager, "execv", execv_detour, FnExecv, FN_EXECV);
        }
    }

    #[cfg(target_os = "macos")]
    unsafe {
        replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
    }
}

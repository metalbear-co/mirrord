#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString};

use frida_gum::interceptor::Interceptor;
use libc::{c_char, c_int};
use mirrord_layer_macro::hook_guard_fn;
use mirrord_sip::sip_patch;
use null_terminated::Nul;
use tracing::{debug, warn};

use crate::{
    detour::{
        Bypass::{AgentAlreadyInEnv, NoSipDetected},
        Detour,
        Detour::{Bypass, Error, Success},
    },
    error::{
        HookError,
        HookError::{Null, NullPointer},
    },
    file::ops::str_from_rawish,
    replace,
};

pub(crate) unsafe fn enable_execve_hook(interceptor: &mut Interceptor) {
    let _ = replace!(interceptor, "execve", execve_detour, FnExecve, FN_EXECVE);
}

/// Check if the file that is to be executed has SIP and patch it if it does.
#[tracing::instrument(level = "trace")]
pub(super) fn patch_if_sip(rawish_path: Option<&CStr>) -> Detour<String> {
    let path = str_from_rawish(rawish_path)?;
    match sip_patch(path) {
        Ok(None) => Bypass(NoSipDetected(path.to_string())),
        Ok(Some(new_path)) => Success(new_path),
        Err(sip_error) => {
            warn!(
                "The application executed the program {} which mirrord tried to check for SIP and \
                patch if necessary. However the SIP patch failed with the error: {:?}, so mirrord \
                did not load into it, and all operations in that program will be executed locally.",
                path, sip_error
            );
            Error(HookError::FailedSipPatch(sip_error))
        }
    }
}

/// Does the null terminated pointer array `env_arr` contain an env var named `var_name`.
fn env_contains_var(env_arr: &Nul<*const c_char>, var_name: &str) -> bool {
    // The array is not checked to actually be null terminated. So limit number of taken args.
    let max_env_vars_num = 1024;
    let prefix = var_name.to_owned() + "=";
    env_arr
        .iter()
        .take(max_env_vars_num)
        .map(|ptr| unsafe { CStr::from_ptr(ptr.clone()) }) // Can't be null.
        .map(Some) // str_from_rawish takes an option.
        .map(str_from_rawish)
        .any(|detour_str| {
            detour_str
                .map(|k_v| k_v.starts_with(&prefix))
                .unwrap_or(false)
        })
}

/// Check if array contains the agent vars, if not return pointer to new array that does.
unsafe fn add_exiting_agent_if_missing(envp: *const *const c_char) -> Detour<Vec<String>> {
    if envp == (0 as *const *const c_char) {
        // Not allowed on macos.
        return Error(NullPointer);
    }
    let env_arr = Nul::new_unchecked(envp);
    if env_contains_var(env_arr, "MIRRORD_CONNECT_AGENT") {
        Bypass(AgentAlreadyInEnv)
    } else {
        Success(vec!["TODO".to_string()]) // TODO: remove this line
                                          // get_new_envp_with_agent(env_arr) // TODO: uncomment.
    }
}

/// Hook for `libc::execve`.
///
/// Patch file if it is SIPed, then call normal execve (on the patched file if patched, otherwise
/// on original file)
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // TODO: DELETE
    debug!("Hooked execve!");
    // Do unsafe part of path conversion here.
    let rawish_path = (!path.is_null()).then(|| CStr::from_ptr(path));
    let mut patched_path = CString::default();
    let final_path = patch_if_sip(rawish_path)
        .and_then(|s| match CString::new(s) {
            Ok(c_string) => {
                patched_path = c_string;
                Success(patched_path.as_ptr())
            }
            Err(err) => Error(Null(err)),
        })
        .unwrap_or(path); // Continue even if there were errors - just run without patching.
    let mut ptr_vec: Vec<*const c_char> = Vec::new();
    let final_envp = add_exiting_agent_if_missing(envp)
        .map(|string_vec| {
            string_vec.iter().for_each(|string| {
                if let Ok(c_string) = CString::new(string.to_owned()) {
                    ptr_vec.push(c_string.as_ptr());
                }
            });
            ptr_vec.push(0 as *const c_char); // Push null pointer to terminate array.
            <&Nul<*const c_char> as ::std::convert::TryFrom<_>>::try_from(
                &ptr_vec as &[*const c_char],
            )
            .map(|arr| arr.as_ptr())
            .unwrap() // Infallible
        })
        .unwrap_or(envp);

    FN_EXECVE(final_path, argv, final_envp)
}

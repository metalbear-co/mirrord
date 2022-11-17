#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString};

use frida_gum::interceptor::Interceptor;
use libc::{c_char, c_int};
use mirrord_layer_macro::hook_guard_fn;
use mirrord_sip::{sip_patch, SipError};
use tracing::{trace, warn};

use crate::{
    detour::{
        Bypass::{ExecOnNonExistingFile, NoSipDetected},
        Detour,
        Detour::{Bypass, Error, Success},
    },
    error::{HookError, HookError::Null},
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
        Err(SipError::FileNotFound(non_existing_bin)) => {
            trace!(
                "The application wants to execute {}, SIP check got FileNotFound. If the file \
                actually exists, make sure it is excluded from FS ops.",
                non_existing_bin
            );
            Bypass(ExecOnNonExistingFile(non_existing_bin))
        }
        Err(sip_error) => {
            warn!(
                "The application is trying to execute the program {} which mirrord tried to check \
                for SIP and patch if necessary. However the SIP patch failed with the error: {:?}, \
                so mirrord did not load into it, and all operations in that program will be \
                executed locally if its execution without mirrord indeed succeeds.",
                path, sip_error
            );
            Error(HookError::FailedSipPatch(sip_error))
        }
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

    FN_EXECVE(final_path, argv, envp)
}

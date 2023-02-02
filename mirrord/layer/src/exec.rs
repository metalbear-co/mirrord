#![cfg(target_os = "macos")]

use std::{
    env,
    ffi::{c_void, CStr, CString},
    mem::MaybeUninit,
    ptr::null,
};

use itertools::Itertools;
use libc::{c_char, c_int, pid_t};
use mirrord_layer_macro::hook_guard_fn;
use mirrord_sip::{sip_patch, SipError, MIRRORD_PATCH_DIR};
use null_terminated::Nul;
use tracing::{trace, warn};

use crate::{
    detour::{
        Bypass::{ExecOnNonExistingFile, NoSipDetected, TooManyArgs},
        Detour,
        Detour::{Bypass, Error, Success},
    },
    error::{HookError, HookError::Null},
    file::ops::str_from_rawish,
    hooks::HookManager,
    replace,
};

const MAX_ARGC: usize = 254;

pub(crate) unsafe fn enable_execve_hook(hook_manager: &mut HookManager) {
    replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
    replace!(
        hook_manager,
        "posix_spawn",
        posix_spawn_detour,
        FnPosix_spawn,
        FN_POSIX_SPAWN
    );
}

/// Check if the file that is to be executed has SIP and patch it if it does.
#[tracing::instrument(level = "trace")]
pub(super) fn patch_if_sip(path: &str) -> Detour<String> {
    match sip_patch(path) {
        Ok(None) => Bypass(NoSipDetected(path.to_string())),
        Ok(Some(new_path)) => Success(new_path),
        Err(SipError::FileNotFound(non_existing_bin)) => {
            trace!(
                "The application wants to execute {}, SIP check got FileNotFound for {}. \
                If the file actually exists and should have been found, make sure it is excluded \
                from FS ops.",
                path,
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

fn raw_to_str(raw_str: &*const c_char) -> Detour<&str> {
    let rawish_str = (!raw_str.is_null()).then(|| unsafe { CStr::from_ptr(raw_str.to_owned()) });
    str_from_rawish(rawish_str)
}

/// Check if the arguments to the new executable contain paths to mirrord's temp dir.
/// If they do, create a new array with the original paths instead of the patched paths.
fn intercept_tmp_dir(argv_arr: &Nul<*const c_char>) -> Detour<Vec<MaybeUninit<CString>>> {
    let tmp_dir = env::temp_dir()
        .join(MIRRORD_PATCH_DIR)
        .to_string_lossy()
        .to_string();
    let mut c_string_vec: Vec<MaybeUninit<CString>> = Vec::new();

    // Iterate through args, if an argument is a path inside our temp dir, save pointer to
    // after that prefix instead.
    // Example:
    //                                           "/path-to-mirrord-temp/file1"
    // If argv[1] is a pointer to a string ptr: --^                    ^
    // Then save a pointer to after the temp dir prefix instead:  -----|
    for (i, arg) in argv_arr.iter().enumerate() {
        if i > MAX_ARGC {
            // the iterator will go until there is a null pointer, so stop after MAX_ARGC so
            // that we don't just keep going indefinitely if a bad argv was passed.
            return Bypass(TooManyArgs);
        }
        let arg_str = raw_to_str(arg)?;
        trace!("exec arg: {arg_str}");

        c_string_vec.push(
            MaybeUninit::new(
                CString::new(arg_str
                    .strip_prefix(&tmp_dir)
                    .inspect(|original_path| {
                        trace!(
                            "Intercepted mirrord's temp dir in argv: {}. Replacing with original path: {}.",
                            arg_str,
                            original_path
                        );
                    })
                    .unwrap_or(arg_str) // No temp-dir prefix found, use arg as is.
                    // As the string slice we get here is a slice of memory allocated and managed by
                    // the user app, we copy the data and create new CStrings out of the copy 
                    // without consuming the original data.
                    .to_owned()
                )?
            )
        );
    }
    c_string_vec.push(MaybeUninit::zeroed());
    Success(c_string_vec)
}

/// Patch the new executable for SIP if necessary. Also: if mirrord's temporary directory appears
/// in any of the arguments, remove it and leave only the original path of the file. If for example
/// `argv[1]` is `"/tmp/mirrord-bin/bin/bash"`, create a new `argv` where `argv[1]` is
/// `"/bin/bash"`.
unsafe fn patch_sip_for_new_process(
    path: *const c_char,
    argv: *const *const c_char,
) -> Detour<(CString, Vec<MaybeUninit<CString>>)> {
    let calling_exe = env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    trace!("Executable {} called execve/posix_spawn", calling_exe);

    let path_str = raw_to_str(&path)?;

    let path_c_string = patch_if_sip(path_str)
        .and_then(|new_path| Success(CString::new(new_path)?))
        // Continue also on error, use original path, don't bypass yet, try cleaning argv.
        .unwrap_or(CString::new(path_str.to_string())?);

    let argv_arr = Nul::new_unchecked(argv);

    let argv_vec = intercept_tmp_dir(argv_arr)?;
    Success((path_c_string, argv_vec))
}

/// After we're done with the new vector of CStrings (which are [`MaybeUninit`] so that we can put
/// a null pointer at the end of the vector), we have to make sure they are dropped as [`CString`]s.
///
/// The vector must have only initialized items except for the last item that should be
/// uninitialized.
fn drop_vec(mut mu_c_string_vec: Vec<MaybeUninit<CString>>) {
    mu_c_string_vec.pop(); // Make sure CString::drop is NOT called on the terminating null.
    mu_c_string_vec
        .into_iter()
        .for_each(|mut mu_c_string| unsafe { mu_c_string.assume_init_drop() })
}

/// Hook for `libc::execve`.
///
/// Patch file if it is SIPed, used new path if patched.
/// If any args in argv are paths to mirrord's temp directory, strip the temp dir part.
/// So if argv[1] is "/var/folders/1337/mirrord-bin/opt/homebrew/bin/npx"
/// Switch it to "/opt/homebrew/bin/npx"
/// then call normal execve with the possibly updated path and argv and the original envp.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    match patch_sip_for_new_process(path, argv) {
        Success((new_path, mut new_argv)) => {
            // Here we assume that in the memory - a buffer of `CStrings` is exactly the same
            // as a buffer of the respective `.as_ptr()`s of those `CString`s.
            let res = FN_EXECVE(
                new_path.as_ptr(),
                new_argv.as_ptr() as *const *const c_char,
                envp,
            );

            // When calling drop_vec, we have to be sure that all items in the vector are
            // initialized except for the last. We are sure of that here, because all the vector
            // items are created with MaybeUninit::new, so it is safe to assume they are
            // init, except for the last item which is created as MaybeUninit::zeroed.
            // Without this call, we would leak all the CStrings in the vec.
            drop_vec(new_argv);

            res
        }
        _ => FN_EXECVE(path, argv, envp),
    }
}

// TODO: do we also need to hook posix_spawnp?
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn posix_spawn_detour(
    pid: *const pid_t,
    path: *const c_char,
    file_actions: *const c_void,
    attrp: *const c_void,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    match patch_sip_for_new_process(path, argv) {
        Success((new_path, new_argv)) => {
            let res = FN_POSIX_SPAWN(
                pid,
                new_path.as_ptr(),
                file_actions,
                attrp,
                // Here we assume that in the memory - a buffer of `CStrings` is exactly the same
                // as a buffer of the respective `.as_ptr()`s of those `CString`s.
                new_argv.as_ptr() as *const *const c_char,
                envp,
            );

            // When calling drop_vec, we have to be sure that all items in the vector are
            // initialized. We are sure of that here, because except for the last item of the
            // vector all items are created with MaybeUninit::new, so it is safe to assume they are
            // init. Without this call, we would leak all the CStrings in the vec.
            drop_vec(new_argv);

            res
        }
        _ => FN_POSIX_SPAWN(pid, path, file_actions, attrp, argv, envp),
    }
}

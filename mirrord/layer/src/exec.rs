#![cfg(target_os = "macos")]

use std::{
    env,
    ffi::{c_void, CStr, CString},
};

use libc::{c_char, c_int, pid_t};
use mirrord_layer_macro::hook_guard_fn;
use mirrord_sip::{sip_patch, SipError, TMP_DIR_ENV_VAR_NAME};
use null_terminated::Nul;
use tracing::{error, trace, warn};

use crate::{
    detour::{
        Bypass::{
            ExecOnNonExistingFile, NoSipDetected, NoTempDirInArgv, TempDirEnvVarNotSet, TooManyArgs,
        },
        Detour,
        Detour::{Bypass, Error, Success},
    },
    error::HookError,
    exec::patched_args::PatchedArgs,
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
pub(super) fn patch_if_sip(rawish_path: Option<&CStr>) -> Detour<String> {
    let path = str_from_rawish(rawish_path)?;
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
fn intercept_tmp_dir(argv_arr: &Nul<*const c_char>) -> Detour<Vec<*const c_char>> {
    if let Ok(tmp_dir) = env::var(TMP_DIR_ENV_VAR_NAME) {
        let mut ptr_vec: Vec<*const c_char> = Vec::new();
        let mut changed = false; // Did we change any of the args?

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
            trace!("execve arg: {arg_str}");
            ptr_vec.push(
                    arg_str
                        .strip_prefix(&tmp_dir)
                        .map_or_else(
                            || arg_str.as_ptr() as *const c_char, // No temp dir, use arg as is.
                            |original_path| {
                                trace!("Intercepted mirrord's temp dir in argv: {}. Replacing with original path: {}.", arg_str, original_path);
                                changed = true;
                                original_path.as_ptr() as *const c_char
                            })
            );
        }
        return if changed {
            ptr_vec.push(std::ptr::null::<c_char>()); // Terminate with null.
            Success(ptr_vec)
        } else {
            Bypass(NoTempDirInArgv)
        };
    }
    error!(
        "mirrord internal error: environment variable {} not set.",
        TMP_DIR_ENV_VAR_NAME
    );
    Bypass(TempDirEnvVarNotSet) // Temp dir was not set. Cannot intercept. Should not happen.
}

mod patched_args {
    use std::ffi::CString;

    use libc::c_char;

    /// The arguments for `execve` or `posix_spawn` after patching the for side-stepping SIP.
    /// Since the arguments for those functions are pointers, if we indeed modify the arguments, we
    /// create new objects, so we have to keep them alive, so we put them in this struct, and the
    /// detour code has to hold on to this struct until after the call to the original libc
    /// functions.
    pub struct PatchedArgs {
        original_path: *const c_char,
        path_c_string: Option<CString>,
        original_argv: *const *const c_char,
        argv_vec: Option<Vec<*const c_char>>,
    }

    impl PatchedArgs {
        pub fn new(
            original_path: *const c_char,
            path_c_string: Option<CString>,
            original_argv: *const *const c_char,
            argv_vec: Option<Vec<*const c_char>>,
        ) -> Self {
            Self {
                original_path,
                path_c_string,
                original_argv,
                argv_vec,
            }
        }

        /// Get the (maybe patched) path that should be executed.
        pub fn get_path(&self) -> *const c_char {
            // If we created a new string return a pointer to that, otherwise just the original
            // pointer.
            self.path_c_string
                .as_ref()
                .map(|path| path.as_ptr())
                .unwrap_or(self.original_path)
        }

        /// Get the (maybe changed) argv.
        pub fn get_argv(&self) -> *const *const c_char {
            // If we created a new array return a pointer to that, otherwise just the original
            // pointer.
            self.argv_vec
                .as_ref()
                .map(|argv| argv.as_ptr())
                .unwrap_or(self.original_argv)
        }
    }
}

unsafe fn patch_sip_for_new_process(
    path: *const c_char,
    argv: *const *const c_char,
) -> PatchedArgs {
    let exe_path = env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    trace!("Executable {} called execve/posix_spawn", exe_path);

    // Do unsafe part of path conversion here.
    let rawish_path = (!path.is_null()).then(|| CStr::from_ptr(path));

    // Continue even if there were errors - just run without patching.
    let path_c_string = patch_if_sip(rawish_path)
        .success()
        .map(CString::new)
        .transpose()
        .unwrap_or_default();

    let argv_arr = Nul::new_unchecked(argv);

    // If we intercept args, we create a new array.
    // execve takes a null terminated array of char pointers.
    // ptr_vec will own the vector that will be passed to execve as an array.
    let argv_vec = intercept_tmp_dir(argv_arr).success();

    PatchedArgs::new(path, path_c_string, argv, argv_vec)
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
    let patched_args = patch_sip_for_new_process(path, argv);
    FN_EXECVE(patched_args.get_path(), patched_args.get_argv(), envp)
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
    let patched_args = patch_sip_for_new_process(path, argv);
    FN_POSIX_SPAWN(
        pid,
        patched_args.get_path(),
        file_actions,
        attrp,
        patched_args.get_argv(),
        envp,
    )
}

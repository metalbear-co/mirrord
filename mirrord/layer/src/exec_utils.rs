use std::{
    env,
    ffi::{CStr, CString, c_void},
    path::PathBuf,
    sync::OnceLock,
};

use libc::{c_char, c_int, pid_t};
use mirrord_layer_lib::{
    detour::{
        Bypass::{
            ExecOnNonExistingFile, FileOperationInMirrordBinTempDir, NoSipDetected, TooManyArgs,
        },
        Detour::{self, Bypass, Error, Success},
    },
    error::HookError,
};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_sip::{MIRRORD_PATCH_DIR, SipError, SipPatchOptions, sip_patch};
use null_terminated::Nul;
use tracing::{info, trace, warn};

use crate::{
    EXECUTABLE_ARGS,
    common::{CheckedInto, strip_mirrord_path},
    exec_hooks::{hooks, *},
    graceful_exit,
    hooks::HookManager,
    replace,
};

/// Maximal number of items to expect in argv.
/// If there are more we assume something's wrong and abort the detour (and continue without
/// patching).
const MAX_ARGC: usize = 256;

pub(crate) static PATCH_BINARIES: OnceLock<Vec<String>> = OnceLock::new();

pub(crate) static SKIP_PATCH_BINARIES: OnceLock<Vec<String>> = OnceLock::new();

pub(crate) unsafe fn enable_macos_hooks(
    hook_manager: &mut HookManager,
    patch_binaries: Vec<String>,
    skip_binaries: Vec<String>,
) {
    unsafe {
        PATCH_BINARIES
            .set(patch_binaries)
            .expect("couldn't set patch_binaries");
        SKIP_PATCH_BINARIES
            .set(skip_binaries)
            .expect("couldn't set skip_binaries");
        replace!(
            hook_manager,
            "posix_spawn",
            posix_spawn_detour,
            FnPosix_spawn,
            FN_POSIX_SPAWN
        );
        replace!(
            hook_manager,
            "_NSGetExecutablePath",
            _nsget_executable_path_detour,
            Fn_nsget_executable_path,
            FN__NSGET_EXECUTABLE_PATH
        );
        replace!(hook_manager, "dlopen", dlopen_detour, FnDlopen, FN_DLOPEN);
    }
}

/// Check if the file that is to be executed has SIP and patch it if it does.
#[mirrord_layer_macro::instrument(level = "trace")]
pub(super) fn patch_if_sip(path: &str) -> Detour<String> {
    let patch_binaries = PATCH_BINARIES.get().expect("patch binaries not set");
    let skip_patch_binaries = SKIP_PATCH_BINARIES
        .get()
        .expect("skip patch binaries not set");
    // use args for logging only, so no need to fail if they're not set
    let args = EXECUTABLE_ARGS
        .get()
        .map(|exec_args| exec_args.args.clone());
    let log_info = crate::setup()
        .layer_config()
        .experimental
        .sip_log_destination
        .as_ref()
        .map(|log_destination| mirrord_sip::SipLogInfo {
            log_destination,
            args: args.as_deref(),
            load_type: None,
        });
    match sip_patch(
        path,
        SipPatchOptions {
            patch: patch_binaries,
            skip: skip_patch_binaries,
        },
        log_info,
    ) {
        Ok(None) => Bypass(NoSipDetected(path.to_string())),
        Ok(Some(new_path)) => Success(new_path),
        Err(SipError::FileNotFound(non_existing_bin)) => {
            trace!(
                "The application wants to execute {}, SIP check got FileNotFound for {}. \
                If the file actually exists and should have been found, make sure it is excluded \
                from FS ops.",
                path, non_existing_bin
            );
            Bypass(ExecOnNonExistingFile(non_existing_bin))
        }
        ref sip_error @ Err(SipError::TooManyFilesOpen(..)) => {
            // we can't recover from hitting the fd limit, so we have to exit fully
            graceful_exit!(
                "mirrord failed to patch SIP with: {:?}",
                sip_error.as_ref().unwrap_err()
            );
            // compile error if this match arm does not return a Detour
            unreachable!()
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

/// Check if the arguments to the new executable contain paths to mirrord's temp dir.
/// If they do, create a new array with the original paths instead of the patched paths.
fn intercept_tmp_dir(argv_arr: &Nul<*const c_char>) -> Detour<Argv> {
    let mut c_string_vec = Argv::default();

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
        let arg_str: &str = arg.checked_into()?;
        trace!("exec arg: {arg_str}");

        // SAFETY: We only slice after we find the string in the path
        // so it must be valid
        #[allow(clippy::indexing_slicing)]
        let stripped = arg_str
            .find(MIRRORD_PATCH_DIR)
            .map(|index| &arg_str[(MIRRORD_PATCH_DIR.len() + index)..])
            .inspect(|original_path| {
                trace!(
                    "Intercepted mirrord's temp dir in argv: {}. Replacing with original path: {}.",
                    arg_str, original_path
                );
            })
            .unwrap_or(arg_str) // No temp-dir prefix found, use arg as is.
            // As the string slice we get here is a slice of memory allocated and managed by
            // the user app, we copy the data and create new CStrings out of the copy
            // without consuming the original data.
            .to_owned();

        c_string_vec.push(CString::new(stripped)?)
    }
    Success(c_string_vec)
}

/// Verifies that mirrord environment is passed to child process
fn intercept_environment(envp_arr: &Nul<*const c_char>) -> Detour<Argv> {
    let mut c_string_vec = Argv::default();

    let mut found_dyld = false;
    for arg in envp_arr.iter() {
        let Detour::Success(arg_str): Detour<&str> = arg.checked_into() else {
            tracing::debug!("Failed to convert envp argument to string. Skipping.");
            c_string_vec.push(unsafe { CStr::from_ptr(*arg).to_owned() });
            continue;
        };

        if arg_str.split('=').next() == Some("DYLD_INSERT_LIBRARIES") {
            found_dyld = true;
        }

        c_string_vec.push(CString::new(arg_str)?)
    }

    if !found_dyld {
        for (key, value) in crate::setup().env_backup() {
            c_string_vec.push(CString::new(format!("{key}={value}"))?);
        }
    }
    Success(c_string_vec)
}

/// Patch the new executable for SIP if necessary. Also: if mirrord's temporary directory appears
/// in any of the arguments, remove it and leave only the original path of the file. If for example
/// `argv[1]` is `"/tmp/mirrord-bin/bin/bash"`, create a new `argv` where `argv[1]` is
/// `"/bin/bash"`.
#[tracing::instrument(level = "trace", skip_all, ret)]
pub(crate) unsafe fn patch_sip_for_new_process(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> Detour<(CString, Argv, Argv)> {
    unsafe {
        let calling_exe = env::current_exe()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        trace!("Executable {} called execve/posix_spawn", calling_exe);

        let path_str = path.checked_into()?;
        // If an application is trying to run an executable from our tmp dir, strip our tmp dir from
        // the path. The file might not even exist in our tmp dir, and the application is
        // expecting it there only because it somehow found out about its own patched
        // location in our tmp dir. If original path is SIP, and actually exists in our dir
        // that patched executable will be used.
        let path_str = strip_mirrord_path(path_str).unwrap_or(path_str);
        let path_c_string = patch_if_sip(path_str)
            .and_then(|new_path| Success(CString::new(new_path)?))
            // Continue also on error, use original path, don't bypass yet, try cleaning argv.
            .unwrap_or(CString::new(path_str.to_string())?);

        let argv_arr = Nul::new_unchecked(argv);
        let envp_arr = Nul::new_unchecked(envp);

        let argv_vec = intercept_tmp_dir(argv_arr)?;
        let envp_vec = intercept_environment(envp_arr)?;
        Success((path_c_string, argv_vec, envp_vec))
    }
}

/// Hook for `libc::posix_spawn`.
/// Same as `execve_detour`, with all the extra arguments present here being passed untouched.
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
    unsafe {
        match patch_sip_for_new_process(path, argv, envp) {
            Detour::Success((path, argv, envp)) => {
                match hooks::prepare_execve_envp(Detour::Success(envp.clone())) {
                    Detour::Success(envp) => FN_POSIX_SPAWN(
                        pid,
                        path.into_raw().cast_const(),
                        file_actions,
                        attrp,
                        argv.leak(),
                        envp.leak(),
                    ),
                    _ => FN_POSIX_SPAWN(
                        pid,
                        path.into_raw().cast_const(),
                        file_actions,
                        attrp,
                        argv.leak(),
                        envp.leak(),
                    ),
                }
            }
            _ => FN_POSIX_SPAWN(pid, path, file_actions, attrp, argv, envp),
        }
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn _nsget_executable_path_detour(
    path: *mut c_char,
    buflen: *mut u32,
) -> c_int {
    unsafe {
        let res = FN__NSGET_EXECUTABLE_PATH(path, buflen);
        if res == 0 {
            let path_buf_detour = CheckedInto::<PathBuf>::checked_into(path as *const c_char);
            if let Bypass(FileOperationInMirrordBinTempDir(later_ptr)) = path_buf_detour {
                // SAFETY: If we're here, the original function was passed this pointer and was
                //         successful, so this pointer must be valid.
                let old_len = *buflen;

                // SAFETY:  `later_ptr` is a pointer to a later char in the same buffer.
                let prefix_len = later_ptr.offset_from(path);

                let stripped_len = old_len - prefix_len as u32;

                // SAFETY:
                // - can read `stripped_len` bytes from `path_cstring` because it's its length.
                // - can write `stripped_len` bytes to `path`, because the length of the path after
                //   stripping a prefix will always be shorter than before.
                // - cannot use `copy_from_nonoverlapping`.
                path.copy_from(later_ptr, stripped_len as _);

                // SAFETY:
                // - We call the original function before this, so if it's not a valid pointer we
                //   should not get back 0, and then this code is not executed.
                *buflen = stripped_len;

                // If the buffer is long enough for the path, it is long enough for the stripped
                // path.
                return 0;
            }
        }
        res
    }
}

/// Just strip the sip patch dir out of the path if there.
/// Don't use guard since we want the original function to be able to call back to our detours.
/// For example, `dlopen` loads library `funlibrary.dylib` which then calls `dlopen` as part
/// of it's initialization sequence, causing the second dlopen to fail since we don't patch
/// the path for the second call.
#[hook_fn]
pub(crate) unsafe extern "C" fn dlopen_detour(
    raw_path: *const c_char,
    mode: c_int,
) -> *const c_void {
    unsafe {
        // we hold the guard manually for tracing/internal code
        let guard = mirrord_layer_lib::detour::DetourGuard::new();
        let detour: Detour<PathBuf> = raw_path.checked_into();
        let raw_path = if let Bypass(FileOperationInMirrordBinTempDir(ptr)) = detour {
            trace!("dlopen called with a path inside our patch dir, switching with fixed pointer.");
            ptr
        } else {
            trace!("dlopen called on path {detour:?}.");
            raw_path
        };
        drop(guard);
        // call dlopen guardless
        FN_DLOPEN(raw_path, mode)
    }
}

/// Skip all entries in `envp`, extract and return all apple variables.
pub(crate) unsafe fn extract_applev() -> *mut *mut c_char {
    unsafe extern "C" {
        static mut environ: *mut *mut c_char;
    }

    unsafe {
        let mut envp = environ;
        let mut envc: usize = 0;

        // skip through all envs in envp
        while !(*envp).is_null() {
            envp = envp.add(1);
            envc = envc.saturating_add(1);
        }

        // skip the NULL after envs
        envp = envp.add(1);

        info!("skipped {} envs from envp", envc);

        envp
    }
}

#[cfg(all(test, target_os = "macos"))]
mod test {
    use std::ffi::CStr;

    #[test]
    fn test_extract_applev() {
        unsafe {
            let applev = super::extract_applev();
            if applev.is_null() {
                panic!("applev is null");
            } else {
                // first value in applev is exec path
                assert!(
                    CStr::from_ptr(*applev)
                        .to_string_lossy()
                        .contains("executable_path=")
                );
            }
        }
    }
}

#![cfg(target_os = "macos")]

use std::{
    env,
    ffi::{c_void, CString},
    marker::PhantomData,
    ptr,
    sync::OnceLock,
};

use libc::{c_char, c_int, pid_t};
use mirrord_layer_macro::hook_guard_fn;
use mirrord_sip::{sip_patch, SipError, MIRRORD_PATCH_DIR};
use null_terminated::Nul;
use tracing::{trace, warn};

use crate::{
    common::CheckedInto,
    detour::{
        Bypass::{ExecOnNonExistingFile, NoSipDetected, TooManyArgs},
        Detour,
        Detour::{Bypass, Error, Success},
    },
    error::HookError,
    hooks::HookManager,
    replace,
};

/// Maximal number of items to expect in argv.
/// If there are more we assume something's wrong and abort the detour (and continue without
/// patching).
const MAX_ARGC: usize = 256;

pub(crate) static PATCH_BINARIES: OnceLock<Vec<String>> = OnceLock::new();

pub(crate) unsafe fn enable_execve_hook(
    hook_manager: &mut HookManager,
    patch_binaries: Vec<String>,
) {
    PATCH_BINARIES
        .set(patch_binaries)
        .expect("couldn't set patch_binaries");
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
    let patch_binaries = PATCH_BINARIES.get().expect("patch binaries not set");
    match sip_patch(path, patch_binaries) {
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

/// Hold a vector of new CStrings to use instead of the original argv.
#[derive(Default)]
struct Argv(Vec<CString>);

/// This must be memory-same as just a `*const c_char`.
#[repr(C)]
struct StringPtr<'a> {
    ptr: *const c_char,
    _phantom: PhantomData<&'a ()>,
}

impl Argv {
    /// Get a null-pointer [`StringPtr`].
    fn null_string_ptr() -> StringPtr<'static> {
        StringPtr {
            ptr: ptr::null(),
            _phantom: Default::default(),
        }
    }

    /// Get a vector of pointers of which the data buffer is memory-same as a null-terminated array
    /// of pointers to null-terminated strings.
    fn null_vec(&self) -> Vec<StringPtr> {
        let mut vec: Vec<StringPtr> = self
            .0
            .iter()
            .map(|c_string| StringPtr {
                ptr: c_string.as_ptr(),
                _phantom: Default::default(),
            })
            .collect();
        vec.push(Self::null_string_ptr());
        vec
    }
}

/// Check if the arguments to the new executable contain paths to mirrord's temp dir.
/// If they do, create a new array with the original paths instead of the patched paths.
fn intercept_tmp_dir(argv_arr: &Nul<*const c_char>) -> Detour<Argv> {
    let tmp_dir = env::temp_dir()
        .join(MIRRORD_PATCH_DIR)
        .to_string_lossy()
        .to_string();
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
        let arg_str: &str = (*arg).checked_into()?;
        trace!("exec arg: {arg_str}");

        c_string_vec.0.push(
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
    }
    Success(c_string_vec)
}

/// Patch the new executable for SIP if necessary. Also: if mirrord's temporary directory appears
/// in any of the arguments, remove it and leave only the original path of the file. If for example
/// `argv[1]` is `"/tmp/mirrord-bin/bin/bash"`, create a new `argv` where `argv[1]` is
/// `"/bin/bash"`.
unsafe fn patch_sip_for_new_process(
    path: *const c_char,
    argv: *const *const c_char,
) -> Detour<(CString, Argv)> {
    let calling_exe = env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    trace!("Executable {} called execve/posix_spawn", calling_exe);

    let path_str = path.checked_into()?;

    let path_c_string = patch_if_sip(path_str)
        .and_then(|new_path| Success(CString::new(new_path)?))
        // Continue also on error, use original path, don't bypass yet, try cleaning argv.
        .unwrap_or(CString::new(path_str.to_string())?);

    let argv_arr = Nul::new_unchecked(argv);

    let argv_vec = intercept_tmp_dir(argv_arr)?;
    Success((path_c_string, argv_vec))
}

/// Hook for `libc::execve`.
///
/// We change 2 arguments and then call the original functions:
/// 1. The executable path - we check it for SIP, create a patched binary and use the path to the
///     new path instead of the original path. If there is no SIP, we use a new string with the
///     same path.
/// 2. argv - we strip mirrord's temporary directory from the start of arguments.
///     So if argv[1] is "/var/folders/1337/mirrord-bin/opt/homebrew/bin/npx"
///     Switch it to "/opt/homebrew/bin/npx"
///     Also here we create a new array with pointers to new strings, even if there are no changes
///     needed (except for the case of an error).
///
/// If there is an error in the detour, we don't exit or anything, we just call the original libc
/// function with the original passed arguments.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    match patch_sip_for_new_process(path, argv) {
        Success((new_path, new_argv)) => {
            let new_argv = new_argv.null_vec();
            FN_EXECVE(
                new_path.as_ptr(),
                new_argv.as_ptr() as *const *const c_char,
                envp,
            )
        }
        _ => FN_EXECVE(path, argv, envp),
    }
}

/// Hook for `libc::posix_spawn`.
/// Same as [`execve_detour`], with all the extra arguments present here being passed untouched.
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
            let new_argv = new_argv.null_vec();
            FN_POSIX_SPAWN(
                pid,
                new_path.as_ptr(),
                file_actions,
                attrp,
                new_argv.as_ptr() as *const *const c_char,
                envp,
            )
        }
        _ => FN_POSIX_SPAWN(pid, path, file_actions, attrp, argv, envp),
    }
}

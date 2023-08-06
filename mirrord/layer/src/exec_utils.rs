#![cfg(target_os = "macos")]

use std::{
    env,
    ffi::{c_void, CString},
    marker::PhantomData,
    path::PathBuf,
    ptr,
    sync::OnceLock,
};

use libc::{c_char, c_int, pid_t};
use mirrord_layer_macro::hook_guard_fn;
use mirrord_sip::{
    sip_patch, SipError, MIRRORD_TEMP_BIN_DIR_CANONIC_STRING, MIRRORD_TEMP_BIN_DIR_STRING,
};
use null_terminated::Nul;
use tracing::{trace, warn};

use crate::{
    common::CheckedInto,
    detour::{
        Bypass::{
            ExecOnNonExistingFile, FileOperationInMirrordBinTempDir, NoSipDetected, TooManyArgs,
        },
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
    replace!(
        hook_manager,
        "_NSGetExecutablePath",
        _nsget_executable_path_detour,
        Fn_nsget_executable_path,
        FN__NSGET_EXECUTABLE_PATH
    );
    replace!(hook_manager, "dlopen", dlopen_detour, FnDlopen, FN_DLOPEN);
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
#[derive(Default, Debug)]
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

        let stripped = arg_str
            .strip_prefix(MIRRORD_TEMP_BIN_DIR_STRING.as_str())
            // If /var/folders... not a prefix, check /private/var/folers...
            .or(arg_str.strip_prefix(MIRRORD_TEMP_BIN_DIR_CANONIC_STRING.as_str()))
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
            .to_owned();

        c_string_vec.0.push(CString::new(stripped)?)
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

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn _nsget_executable_path_detour(
    path: *mut c_char,
    buflen: *mut u32,
) -> c_int {
    let res = FN__NSGET_EXECUTABLE_PATH(path, buflen);
    if res == 0 {
        let path_buf_detour = CheckedInto::<PathBuf>::checked_into(path as *const c_char);
        if let Bypass(FileOperationInMirrordBinTempDir(later_ptr)) = path_buf_detour {
            // SAFETY: If we're here, the original function was passed this pointer and was
            //         successful, so this pointer must be valid.
            let old_len = *buflen;

            // SAFETY:  `later_ptr` is a pointer to a later char in the same buffer.
            let prefix_len = later_ptr.as_ptr().offset_from(path);

            let stripped_len = old_len - prefix_len as u32;

            // SAFETY:
            // - can read `stripped_len` bytes from `path_cstring` because it's its length.
            // - can write `stripped_len` bytes to `path`, because the length of the path after
            //   stripping a prefix will always be shorter than before.
            path.copy_from(later_ptr.as_ptr(), stripped_len as _);

            // SAFETY:
            // - We call the original function before this, so if it's not a valid pointer we should
            //   not get back 0, and then this code is not executed.
            *buflen = stripped_len;

            // If the buffer is long enough for the path, it is long enough for the stripped
            // path.
            return 0;
        }
    }
    res
}

/// Just strip the sip patch dir out of the path if there.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn dlopen_detour(
    raw_path: *const c_char,
    mode: c_int,
) -> *const c_void {
    let detour: Detour<PathBuf> = raw_path.checked_into();
    let raw_path = if let Bypass(FileOperationInMirrordBinTempDir(ptr)) = detour {
        trace!("dlopen called with a path inside our patch dir, switching with fixed pointer.");
        ptr.as_ptr()
    } else {
        trace!("dlopen called on path {detour:?}.");
        raw_path
    };
    FN_DLOPEN(raw_path, mode)
}

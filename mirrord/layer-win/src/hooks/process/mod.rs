//! Windows process creation hooks module.
//!
//! This module provides hooks for intercepting Windows process creation
//! and injecting the mirrord layer DLL into child processes for transparent
//! system call interception.

use std::sync::OnceLock;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    detour::LastErrorGuard,
    error::{LayerError, LayerResult, windows::WindowsError},
    process::windows::{
        environment::WindowsEnv,
        execution::{
            CreateProcessInternalWType, LaunchError, LayerFailurePolicy, LayerManagedProcess,
        },
        injection::{InjectionMethod, MIRRORD_INJECTION_METHOD_ENV},
    },
};
use winapi::{
    ctypes::c_void,
    shared::{
        minwindef::{BOOL, DWORD, FALSE, HMODULE, LPVOID, TRUE},
        ntdef::{HANDLE, LPCWSTR, LPWSTR},
        winerror::ERROR_DLL_INIT_FAILED,
    },
    um::{
        errhandlingapi::SetLastError,
        minwinbase::LPSECURITY_ATTRIBUTES,
        processthreadsapi::{
            LPPROCESS_INFORMATION, LPSTARTUPINFOW, PROCESS_INFORMATION, STARTUPINFOW,
        },
        winnt::PHANDLE,
    },
};

use crate::{
    apply_hook,
    hooks::internal_thread::InternalGuard,
    process::{environment::parse_caller_environment, get_module_name},
};

static CREATE_PROCESS_INTERNAL_W_ORIGINAL: OnceLock<&CreateProcessInternalWType> = OnceLock::new();

/// The method that loaded this layer, read once from its environment.
static LAYER_INJECTION_METHOD: OnceLock<InjectionMethod> = OnceLock::new();

/// The method that loaded this layer, read from its environment on first use and cached, for the
/// children it creates.
///
/// A missing or invalid value means the default: a process mirrord did not create (an attach
/// target) carries none, and an invalid one must not leave the children without mirrord.
pub(crate) fn init_layer_injection_method() -> InjectionMethod {
    *LAYER_INJECTION_METHOD.get_or_init(|| {
        let Ok(value) = std::env::var(MIRRORD_INJECTION_METHOD_ENV) else {
            return InjectionMethod::default();
        };
        InjectionMethod::parse(&value).unwrap_or_else(|error| {
            tracing::error!(
                %error,
                default = %InjectionMethod::default(),
                "{MIRRORD_INJECTION_METHOD_ENV} is invalid, so child processes are injected with the default method"
            );
            InjectionMethod::default()
        })
    })
}

// LoadLibrary hook to detect module loading during injection
type LoadLibraryWType = unsafe extern "system" fn(lpLibFileName: *const u16) -> HMODULE;
static LOAD_LIBRARY_W_ORIGINAL: OnceLock<&LoadLibraryWType> = OnceLock::new();

// GetProcAddress hook to detect API usage during injection
type GetProcAddressType =
    unsafe extern "system" fn(hModule: HMODULE, lpProcName: *const i8) -> *mut c_void;
static GET_PROC_ADDRESS_ORIGINAL: OnceLock<&GetProcAddressType> = OnceLock::new();

/// The program a `CreateProcessInternalW` call starts, as the log names it.
///
/// Only the program, never its arguments: a command line can carry secrets (`curl -H
/// "Authorization: Bearer ..."`), and the log goes into the crash bundle.
///
/// # Arguments
///
/// * `application` - the caller's application name, which names the program when it is given.
/// * `command_line` - the caller's command line. Without an application name, its first token is
///   the program: up to the closing quote when it starts with one, otherwise up to the first space
///   or tab. An unquoted path with spaces is cut at the first space, as Windows first tries it.
///
/// # Returns
///
/// The program, or `None` when the caller gave neither.
fn program_name(application: Option<String>, command_line: Option<&str>) -> Option<String> {
    if application.is_some() {
        return application;
    }

    let command_line = command_line?.trim_start_matches([' ', '\t']);
    let program = match command_line.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next(),
        None => command_line.split([' ', '\t']).next(),
    };

    program
        .filter(|program| !program.is_empty())
        .map(str::to_owned)
}

/// Reads one of the caller's null-terminated wide arguments, for the log only.
///
/// # Safety
///
/// `argument` must be null, or a pointer to a null-terminated wide string that stays valid for
/// this call. Every `CreateProcessInternalW` argument this is used on carries that contract.
///
/// # Arguments
///
/// * `argument` - an `LPCWSTR` the caller passed. Every one of these is allowed to be null.
///
/// # Returns
///
/// The text, or `None` for a null pointer.
unsafe fn wide_argument(argument: *const u16) -> Option<String> {
    (!argument.is_null()).then(|| unsafe { str_win::u16_ptr_to_string(argument) })
}

/// Windows API hook for CreateProcessInternalW function.
///
/// This function intercepts calls to the internal Windows process creation API and redirects
/// them through our unified mirrord process creation system.
///
/// Only a failure before the process exists falls back to the original implementation. Once
/// the process was created, it is never created again, whatever happened to mirrord's layer in
/// it: the program may already have run, and a second creation would run its side effects
/// twice. A process mirrord could not set up runs without mirrord and is reported to the crash
/// monitor (see [`LayerFailurePolicy::RunWithoutMirrord`]). One that could not be allowed to run
/// at all was ended, and the caller gets `ERROR_DLL_INIT_FAILED`.
unsafe extern "system" fn create_process_internal_w_hook(
    user_token: HANDLE,
    application_name: LPCWSTR,
    command_line: LPWSTR,
    process_attributes: LPSECURITY_ATTRIBUTES,
    thread_attributes: LPSECURITY_ATTRIBUTES,
    inherit_handles: BOOL,
    creation_flags: DWORD,
    environment: LPVOID,
    current_directory: LPCWSTR,
    startup_info: LPSTARTUPINFOW,
    process_information: LPPROCESS_INFORMATION,
    restricted_user_token: PHANDLE,
) -> BOOL {
    // Get the original function pointer
    let original = CREATE_PROCESS_INTERNAL_W_ORIGINAL.get().unwrap();

    // Parse environment from Windows API call - check creation flags for format
    let env_vars = unsafe { parse_caller_environment(environment as *mut _, creation_flags) };
    // A creator that sets the variable chooses for its child's tree; an invalid value keeps ours.
    let injection_method = env_vars
        .as_ref()
        .and_then(|env| env.get(MIRRORD_INJECTION_METHOD_ENV))
        .and_then(|value| InjectionMethod::parse(value).ok())
        .unwrap_or_else(init_layer_injection_method);

    // The program and where it starts, because a bundle has to say which program failed to
    // start: "The system cannot find the file specified" means little without what it was trying
    // to run. Its arguments stay out of the log (see `program_name`).
    //
    // The string conversions in a hook this hot are affordable because `tracing` builds a field
    // only when something is listening, and nothing listens at debug in a default run.
    tracing::debug!(
        parent_pid = std::process::id(),
        env_count = ?env_vars.as_ref().map(|environment| environment.len()),
        program = ?program_name(
            unsafe { wide_argument(application_name) },
            unsafe { wide_argument(command_line) }.as_deref(),
        ),
        current_directory = ?unsafe { wide_argument(current_directory) },
        creation_flags = format!("{creation_flags:#010x}"),
        inherit_handles = inherit_handles != 0,
        "CreateProcessInternalW: creating a process"
    );

    // Execute process using closure to preserve all original parameters
    // This ensures perfect fidelity to the original CreateProcessInternalW call
    let create_process_fn = |adjusted_creation_flags,
                             adjusted_environment,
                             adjusted_startup_info: &mut STARTUPINFOW| {
        // Create the process suspended with all original and processed parameters
        let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        let success = unsafe {
            original(
                user_token,
                application_name,
                command_line,
                process_attributes,
                thread_attributes,
                inherit_handles,
                adjusted_creation_flags,
                adjusted_environment,
                current_directory,
                adjusted_startup_info,
                &mut process_info,
                restricted_user_token,
            )
        };

        if success != 0 {
            Ok(process_info)
        } else {
            Err(LayerError::WindowsProcessCreation(
                WindowsError::last_error(),
            ))
        }
    };

    // The injection work reads the layer DLL, resolves exports and may report to the crash
    // monitor, all on this application thread. That is mirrord's own traffic and must reach the
    // local disk and network directly, not come back through this layer's hooks.
    let launch = || {
        let _internal = InternalGuard::enter();
        LayerManagedProcess::execute_with_closure(
            &WindowsEnv::inherited(),
            env_vars,
            injection_method,
            creation_flags,
            unsafe { &mut *startup_info },
            create_process_fn,
            // Hooked child processes are released to run independently; the root
            // pitm/exec job already owns the whole descendant tree via inheritance.
            false,
            LayerFailurePolicy::RunWithoutMirrord,
            None::<mirrord_progress::NullProgress>, // No progress in hook context
        )
        .map(LayerManagedProcess::release)
    };
    let original_call = || unsafe {
        original(
            user_token,
            application_name,
            command_line,
            process_attributes,
            thread_attributes,
            inherit_handles,
            creation_flags,
            environment,
            current_directory,
            startup_info,
            process_information,
            restricted_user_token,
        )
    };

    answer_create_process(launch, original_call, process_information)
}

/// Answers a `CreateProcessInternalW` caller from mirrord's launch, falling back to the original
/// call only when the launch failed before the process existed.
///
/// Once the process was created, it is never created again, whatever happened to mirrord's layer
/// in it: the program may already have run, and a second creation would run its side effects
/// twice.
///
/// # Arguments
///
/// * `launch` - mirrord's launch, which creates the process at most once.
/// * `original` - the original call with the caller's own arguments, for the fallback.
/// * `process_information` - the caller's output parameter, written only on success.
fn answer_create_process(
    launch: impl FnOnce() -> Result<PROCESS_INFORMATION, LaunchError>,
    original: impl FnOnce() -> BOOL,
    process_information: LPPROCESS_INFORMATION,
) -> BOOL {
    match launch() {
        Ok(proc_info) => {
            // SAFETY: the caller's output parameter, valid for this call.
            unsafe { *process_information = proc_info };
            tracing::debug!("Hook succeeded via unified creation");
            TRUE
        }
        Err(LaunchError {
            error,
            created: true,
        }) => {
            // The process existed and has been ended, because it could not be allowed to run.
            // Creating it again is exactly what must not happen, so the caller gets a failure.
            tracing::error!(%error, "the child could not run safely after injection, so it was ended");
            unsafe { SetLastError(ERROR_DLL_INIT_FAILED) };
            FALSE
        }
        Err(LaunchError {
            error,
            created: false,
        }) => {
            // Failure: log error and fall back to original implementation
            tracing::error!("Unified process creation failed: {}", error);
            // Fallback to original Windows API implementation
            tracing::warn!("Falling back to original CreateProcessInternalW");

            let result = original();

            if result != 0 {
                tracing::debug!("Fallback to original API succeeded");
            } else {
                tracing::error!("Both unified creation and fallback failed");
            }

            result
        }
    }
}

/// Hook LoadLibraryW for API monitoring during injection
///
/// This detour monitors DLL loading to provide comprehensive visibility into
/// what modules are being loaded during process injection. Useful for debugging
/// complex injection scenarios and understanding the full API landscape.
unsafe extern "system" fn loadlibrary_w_detour(lpLibFileName: *const u16) -> HMODULE {
    // Call the original LoadLibraryW first
    let original = LOAD_LIBRARY_W_ORIGINAL.get().unwrap();
    let result = unsafe { original(lpLibFileName) };

    // Log LoadLibrary calls for debugging
    if !lpLibFileName.is_null() {
        // Convert the wide string to a Rust string for logging
        let lib_name = unsafe { str_win::u16_ptr_to_string(lpLibFileName) };

        if !lib_name.is_empty() {
            // Trace library loading for debugging
            tracing::trace!("LoadLibraryW: module='{}' handle={:?}", lib_name, result);
        }
    }

    result
}

/// Hook GetProcAddress for API monitoring during injection
///
/// This detour monitors function pointer acquisition to provide comprehensive
/// visibility into what APIs are being resolved during process injection.
/// Essential for understanding the complete API usage pattern.
unsafe extern "system" fn getprocaddress_detour(
    hModule: HMODULE,
    lpProcName: *const i8,
) -> *mut c_void {
    let original = GET_PROC_ADDRESS_ORIGINAL.get().unwrap();

    // Always call original first to get the function address
    let original_result = unsafe { original(hModule, lpProcName) };

    // NOTE(gabriela): lpProcName may be either an ordinal, or pointer to string.
    // NOTE(gabriela): check win-57
    if !lpProcName.is_null() {
        // NOTE(gabriela): convoluted explication below...
        //
        // `#define MAKEINTRESOURCEA(i) ((LPSTR)((ULONG_PTR)((WORD)(i))))``
        // MAKEINTRESOURCEA only keeps the LOWORD of the input.
        //
        // also check out PE spec https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#export-ordinal-table
        // "A 16-bit ordinal number. This field is used only if the Ordinal/Name Flag bit field is 1
        // (import by ordinal). Bits 30-15 or 62-15 must be 0."
        //

        let is_ordinal = !lpProcName.is_null() && (lpProcName as usize) <= u16::MAX as usize;

        let ordinal = is_ordinal.then_some(lpProcName as u16);

        // The module name is a field, computed with a Win32 call before the subscriber runs, so
        // the subscriber cannot keep the error `GetProcAddress` left for its caller.
        let _last_error = LastErrorGuard::save();

        if let Some(number) = ordinal {
            tracing::trace!(
                "GetProcAddress: module={:?} ptr={:?} ordinal='{}' address={:?}",
                get_module_name(hModule as _),
                hModule,
                number,
                original_result
            );
        } else {
            // Convert the function name to a string for logging
            let function_name = unsafe { str_win::u8_ptr_to_string(lpProcName) };

            if !function_name.is_empty() {
                // Trace function resolution for debugging
                tracing::trace!(
                    "GetProcAddress: module={:?} ptr={:?} function='{}' address={:?}",
                    get_module_name(hModule as _),
                    hModule,
                    function_name,
                    original_result
                );
            }
        }
    }

    original_result
}

/// Initialize process creation hooks.
///
/// Installs the CreateProcessInternalW hook using the detours library to intercept
/// all process creation calls and redirect them through our unified mirrord system.
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> LayerResult<()> {
    // NOTE(gabriela): handling this at syscall level is super cumbersome
    // and undocumented, so I'd have to reverse engineer CreateProcessInternalW
    // which is like, 3000-ish lines without type fixups, and we shipped this before
    // and in like, 5 years, we had no issues (previous job).
    // so it should work here too.

    // Snapshot the original prologues before we install any hook. A foreign prologue here is
    // someone else's hook.
    crate::diagnostics::log_prologues();

    apply_hook!(
        guard,
        "kernelbase",
        "CreateProcessInternalW",
        create_process_internal_w_hook,
        CreateProcessInternalWType,
        CREATE_PROCESS_INTERNAL_W_ORIGINAL
    )?;

    // API monitoring hooks for debugging injection scenarios
    tracing::debug!("Installing API tracing hooks for LoadLibraryW and GetProcAddress");

    apply_hook!(
        guard,
        "kernel32",
        "LoadLibraryW",
        loadlibrary_w_detour,
        LoadLibraryWType,
        LOAD_LIBRARY_W_ORIGINAL
    )?;

    apply_hook!(
        guard,
        "kernel32",
        "GetProcAddress",
        getprocaddress_detour,
        GetProcAddressType,
        GET_PROC_ADDRESS_ORIGINAL
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use winapi::{
        shared::winerror::ERROR_ACCESS_DENIED,
        um::{errhandlingapi::GetLastError, winbase::CREATE_UNICODE_ENVIRONMENT},
    };

    use super::*;

    /// A process that was created is never created a second time, not even after mirrord could
    /// not let it run.
    #[test]
    fn a_created_process_is_not_created_again() {
        let launches = Cell::new(0);
        let originals = Cell::new(0);
        let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let answered = answer_create_process(
            || {
                launches.set(launches.get() + 1);
                Err(LaunchError {
                    error: LayerError::DllInjection("test".to_owned()),
                    created: true,
                })
            },
            || {
                originals.set(originals.get() + 1);
                TRUE
            },
            &mut information as LPPROCESS_INFORMATION,
        );
        assert_eq!(answered, FALSE);
        assert_eq!(unsafe { GetLastError() }, ERROR_DLL_INIT_FAILED);
        assert_eq!(
            (launches.get(), originals.get()),
            (1, 0),
            "no second creation"
        );
    }

    /// What one call of [`fake_create_process`] received.
    struct CreateCall {
        environment: LPVOID,
        injection_method: Option<String>,
    }

    thread_local! {
        /// The calls [`fake_create_process`] got on this thread, so tests can run in parallel.
        static CREATE_CALLS: RefCell<Vec<CreateCall>> = const { RefCell::new(Vec::new()) };
    }

    /// Stands for the real `CreateProcessInternalW`: records the call and fails it, as a creation
    /// that Windows refused.
    unsafe extern "system" fn fake_create_process(
        _: HANDLE,
        _: LPCWSTR,
        _: LPWSTR,
        _: LPSECURITY_ATTRIBUTES,
        _: LPSECURITY_ATTRIBUTES,
        _: BOOL,
        creation_flags: DWORD,
        environment: LPVOID,
        _: LPCWSTR,
        _: LPSTARTUPINFOW,
        _: LPPROCESS_INFORMATION,
        _: PHANDLE,
    ) -> BOOL {
        let injection_method =
            unsafe { parse_caller_environment(environment.cast(), creation_flags) }.and_then(
                |environment| {
                    environment
                        .get(MIRRORD_INJECTION_METHOD_ENV)
                        .map(str::to_owned)
                },
            );
        CREATE_CALLS.with_borrow_mut(|calls| {
            calls.push(CreateCall {
                environment,
                injection_method,
            })
        });
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        FALSE
    }

    static FAKE_CREATE_PROCESS: CreateProcessInternalWType = fake_create_process;

    /// Calls the hook with an explicit environment that names `injection_method`, and returns
    /// the method mirrord's launch passed to the child.
    ///
    /// The fake refuses the creation, so the launch fails before any process exists, and the hook
    /// must fall back to the original exactly once, with the caller's own arguments.
    fn launch_method_for(injection_method: &str) -> Option<String> {
        let _ = CREATE_PROCESS_INTERNAL_W_ORIGINAL.set(&FAKE_CREATE_PROCESS);
        CREATE_CALLS.with_borrow_mut(Vec::clear);

        let layer_file = std::env::current_exe()
            .expect("test binary")
            .to_string_lossy()
            .into_owned();
        let mut block = WindowsEnv::from_ordered_entries([
            ("MIRRORD_LAYER_FILE".to_owned(), layer_file),
            (
                MIRRORD_INJECTION_METHOD_ENV.to_owned(),
                injection_method.to_owned(),
            ),
        ])
        .to_block();
        let environment = block.as_mut_ptr() as LPVOID;

        let mut startup_info: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup_info.cb = size_of::<STARTUPINFOW>() as DWORD;
        let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        let answered = unsafe {
            create_process_internal_w_hook(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                FALSE,
                CREATE_UNICODE_ENVIRONMENT,
                environment,
                std::ptr::null(),
                &mut startup_info,
                &mut information,
                std::ptr::null_mut(),
            )
        };

        assert_eq!(answered, FALSE, "the fallback's answer");
        assert_eq!(unsafe { GetLastError() }, ERROR_ACCESS_DENIED);
        CREATE_CALLS.with_borrow_mut(|calls| {
            let [launch, fallback] = calls.as_slice() else {
                panic!("one launch and one fallback, got {} calls", calls.len());
            };
            assert_eq!(
                fallback.environment, environment,
                "the fallback passes the caller's own block"
            );
            assert_ne!(launch.environment, environment);
            launch.injection_method.clone()
        })
    }

    /// A valid method in the creator's block wins; an invalid one keeps the layer's own.
    #[test]
    fn the_hook_falls_back_once_and_lets_the_creator_choose_the_method() {
        assert_eq!(
            launch_method_for("IAT"),
            Some(InjectionMethod::Iat.to_string())
        );
        assert_eq!(
            launch_method_for("bogus"),
            Some(init_layer_injection_method().to_string())
        );
    }

    /// The log names the program and nothing the caller passed it.
    #[test]
    fn only_the_program_is_named() {
        let named = |application: Option<&str>, command_line: Option<&str>| {
            program_name(application.map(str::to_owned), command_line)
        };

        assert_eq!(
            named(
                None,
                Some(r#""C:\Program Files\curl.exe" -H "Authorization: Bearer x""#)
            ),
            Some(r"C:\Program Files\curl.exe".to_owned()),
            "a quoted path ends at its closing quote"
        );
        assert_eq!(
            named(None, Some("  curl.exe\t-H secret")),
            Some("curl.exe".to_owned()),
            "an unquoted program ends at the first space or tab"
        );
        assert_eq!(
            named(Some(r"C:\Windows\cmd.exe"), Some("cmd /c echo secret")),
            Some(r"C:\Windows\cmd.exe".to_owned()),
            "the application name names the program when it is given"
        );
        assert_eq!(named(None, None), None);
        assert_eq!(named(None, Some("   ")), None);
    }
}

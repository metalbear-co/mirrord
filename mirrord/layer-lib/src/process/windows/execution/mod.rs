//! Windows process execution with mirrord layer injection.

use std::{net::SocketAddr, ops::Deref, os::windows::io::BorrowedHandle, path::Path, ptr};

use base64::prelude::*;
use mirrord_config::{
    LayerConfig, MIRRORD_FS_PREFETCH_DIR, MIRRORD_LAYER_CRASH_MONITOR_ADDR,
    MIRRORD_LAYER_CRASH_REPORTING, MIRRORD_LAYER_FULL_MEMORY_DUMP, MIRRORD_LAYER_INTPROXY_ADDR,
    MIRRORD_LAYER_TARGET_CONTAINER_PORTS, MIRRORD_LAYER_WAIT_FOR_DEBUGGER,
};
use stork::{BorrowedTarget, LoadTiming, LoaderState};
use str_win::string_to_u16_buffer;
use utils_win::diagnostics::monitor::InitReport;
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, LPVOID},
        ntdef::{HANDLE, LPCWSTR, LPWSTR},
    },
    um::{
        handleapi::{CloseHandle, SetHandleInformation},
        jobapi2::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject},
        minwinbase::LPSECURITY_ATTRIBUTES,
        processenv::GetStdHandle,
        processthreadsapi::{
            CreateProcessW, GetExitCodeProcess, LPPROCESS_INFORMATION, LPSTARTUPINFOW,
            PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess,
        },
        synchapi::WaitForSingleObject,
        winbase::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, HANDLE_FLAG_INHERIT, INFINITE,
            STARTF_USESTDHANDLES, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            WAIT_OBJECT_0,
        },
        winnt::{
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, PHANDLE,
        },
    },
};

use super::{
    environment::WindowsEnv,
    injection::{InjectionMethod, MIRRORD_INJECTION_METHOD_ENV},
    sync::{InitWaitOutcome, ParentInitEvents},
};
use crate::{
    error::{LayerError, LayerResult, windows::WindowsError},
    logging::MIRRORD_LAYER_LOG_PATH,
    proxy_connection::PROXY_CONNECTION,
    setup::setup,
    socket::{SOCKETS, sockets::SHARED_SOCKETS_ENV_VAR},
};

pub mod debug;

pub use debug::{
    debugger_wait_targets, format_debugger_config, get_current_process_name,
    is_debugger_wait_enabled, should_wait_for_debugger,
};

pub const MIRRORD_AGENT_ADDR_ENV: &str = "MIRRORD_AGENT_ADDR";
pub const MIRRORD_LAYER_ID_ENV: &str = "MIRRORD_LAYER_ID";
pub const MIRRORD_LAYER_FILE_ENV: &str = "MIRRORD_LAYER_FILE";

// Windows-specific child process inheritance environment variables
pub const MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID: &str = "MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID";
pub const MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID: &str = "MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID";
pub const MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR: &str = "MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR";

/// How long a launcher waits for the layer to report ready. Short under test, so the timeout
/// outcomes can be exercised.
#[cfg(not(test))]
const LAYER_INIT_TIMEOUT_MS: u32 = 30_000;
#[cfg(test)]
const LAYER_INIT_TIMEOUT_MS: u32 = 1_500;

/// How long a failed resume waits to see whether the process is ending anyway.
const EXIT_GRACE_MS: u32 = 1_000;

/// Exit codes the Windows loader ends a process with when it cannot finish loading it, by name.
///
/// A heuristic, not a proof: a program may exit with one of these on its own, and the loader can
/// fail for a module that has nothing to do with mirrord. Seen while the layer is loading, it is
/// still the likeliest sign that Windows refused the layer or one of the modules it needs.
const LOADER_STATUSES: &[(u32, &str)] = &[
    (0xC000_007B, "STATUS_INVALID_IMAGE_FORMAT"),
    (0xC000_0135, "STATUS_DLL_NOT_FOUND"),
    (0xC000_0138, "STATUS_ORDINAL_NOT_FOUND"),
    (0xC000_0139, "STATUS_ENTRYPOINT_NOT_FOUND"),
    (0xC000_0142, "STATUS_DLL_INIT_FAILED"),
    (0xC000_0428, "STATUS_INVALID_IMAGE_HASH"),
];

/// The name of a loader exit code. See [`LOADER_STATUSES`].
fn loader_status_name(exit_code: u32) -> Option<&'static str> {
    LOADER_STATUSES
        .iter()
        .find(|(code, _)| *code == exit_code)
        .map(|(_, name)| *name)
}

/// The injector for `method`.
///
/// A layer that waits for a debugger does so inside `DllMain`, which holds a load-library
/// injection's remote thread for as long as attaching takes. Its injector waits for that thread
/// without a limit; every other one keeps stork's default bound.
fn injector_for(method: InjectionMethod, debugger_wait: bool) -> stork::Injector {
    let injector = method.injector();
    if debugger_wait {
        injector.with_remote_wait(None)
    } else {
        injector
    }
}

/// Where a launcher reports a child's startup problems, read from the environment it gave the
/// child rather than from its own: `mirrord exec` and `pitm` keep the monitor's address and the
/// log directory only there.
#[derive(Debug, Default)]
struct StartupReporting {
    /// The child's session crash monitor.
    monitor: Option<SocketAddr>,
    /// The directory the child's layer writes its log to.
    log_dir: Option<String>,
}

impl StartupReporting {
    fn from_environment(environment: &WindowsEnv) -> Self {
        Self {
            monitor: environment
                .get(MIRRORD_LAYER_CRASH_MONITOR_ADDR)
                .and_then(|address| address.parse().ok()),
            log_dir: environment
                .get(MIRRORD_LAYER_LOG_PATH)
                .filter(|directory| !directory.is_empty())
                .map(str::to_owned),
        }
    }

    /// One sentence that tells the reader where the child's layer log is.
    fn log_hint(&self) -> String {
        match &self.log_dir {
            Some(directory) => format!("Its layer log is in {directory}."),
            None => "It wrote no layer log: MIRRORD_LAYER_LOG_PATH was not set.".to_owned(),
        }
    }
}

/// Everything [`LayerManagedProcess::inject_and_resume`] needs besides the process.
struct LaunchPlan<'a> {
    /// The layer to load.
    dll_path: &'a str,
    /// How to load it.
    injection_method: InjectionMethod,
    /// Whether the creator asked for `CREATE_SUSPENDED` itself. Its one suspension is then left
    /// in place, and the process is resumed by the creator, not here.
    caller_suspended: bool,
    /// What to do with a process mirrord could not set up.
    on_layer_failure: LayerFailurePolicy,
    /// Where startup problems are reported.
    reporting: StartupReporting,
    /// Whether the child's layer waits for a debugger, so its startup has no time bound.
    debugger_wait: bool,
}

/// Variables Windows itself needs in every process: without `SystemRoot`, Winsock cannot start,
/// so the child's layer could not reach the internal proxy.
const SYSTEM_VARIABLES: &[&str] = &["SystemRoot", "windir"];

/// Environment variables that should be explicitly forwarded from parent to child process
///
/// A creator that passes its own environment block gets these added to it when the block leaves
/// them out, so they include everything the layer's setup reads: without the internal proxy
/// address, the child's layer fails to start. A value the block already has wins: under
/// `mirrord exec` that block is the session's composed environment, whose log directory, monitor
/// address and proxy address are fresher than the ones in the CLI's own environment.
const FORWARDED_ENV_VARS: &[&str] = &[
    MIRRORD_LAYER_INTPROXY_ADDR,
    MIRRORD_LAYER_TARGET_CONTAINER_PORTS,
    MIRRORD_FS_PREFETCH_DIR,
    MIRRORD_AGENT_ADDR_ENV,
    MIRRORD_LAYER_ID_ENV,
    MIRRORD_LAYER_FILE_ENV,
    MIRRORD_LAYER_LOG_PATH,
    MIRRORD_LAYER_FULL_MEMORY_DUMP,
    MIRRORD_LAYER_CRASH_MONITOR_ADDR,
    MIRRORD_LAYER_CRASH_REPORTING,
    "MIRRORD_LOG",
    "RUST_BACKTRACE",
];

/// Function signature for CreateProcessInternalW Windows API call.
pub type CreateProcessInternalWType = unsafe extern "system" fn(
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
) -> BOOL;

/// What a launcher does with a process it created but could not set mirrord up in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerFailurePolicy {
    /// End the process and return the error.
    ///
    /// For `mirrord exec`: a program without mirrord is not what the user asked for, and nothing
    /// retries the launch.
    Terminate,
    /// Keep the process, without mirrord, and report the failure to the crash monitor.
    ///
    /// For the `CreateProcess` hook: its caller asked for a process and gets one. Answering with a
    /// failure would invite the caller, or the hook's own fallback, to create it again and run its
    /// side effects twice.
    RunWithoutMirrord,
}

/// A failed launch, and whether it got as far as creating the process.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct LaunchError {
    /// What failed. Displayed as the whole message, and not also exposed as a source, so an
    /// error chain does not print it twice.
    pub error: LayerError,
    /// Whether the process had been created. Once it had, it must not be created again; the
    /// process itself is ended when the [`LayerManagedProcess`] holding it drops.
    pub created: bool,
}

pub struct LayerManagedProcess {
    process_info: PROCESS_INFORMATION,
    released: bool,
    terminate_on_drop: bool,
    /// Job object the child is bound to when the caller asked to kill children on
    /// exit (`kill_children_on_exit`). `None` otherwise.
    ///
    /// Holding this handle open ties the child's lifetime to ours: when this process
    /// dies for *any* reason — clean exit, `TerminateProcess`, or a crash — the OS
    /// closes our handles, and the job's `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` limit
    /// then kills the whole child tree.
    ///
    /// `terminate_on_drop` alone can't guarantee this: it fires from `Drop`, which
    /// never runs when the OS kills us abruptly (exactly how IntelliJ/Gradle stop a
    /// run), leaving the layer-loaded child
    /// orphaned — and an orphaned child keeps the agent alive, which surfaces later
    /// as "dirty iptables".
    job: Option<HANDLE>,
}

impl LayerManagedProcess {
    /// Ensure a handle is inheritable for child processes
    fn ensure_handle_inheritable(handle: HANDLE) -> LayerResult<HANDLE> {
        if handle.is_null() {
            return Ok(handle); // Invalid handles can't be made inheritable
        }

        unsafe {
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                // If SetHandleInformation fails, log a warning but continue
                // Some handles (like console handles) might already be inheritable
                tracing::warn!(
                    "Failed to set handle inheritance: {}",
                    WindowsError::last_error()
                );
            }
        }
        Ok(handle)
    }

    /// Add mirrord-specific environment variables to caller's environment.
    ///
    /// # Arguments
    ///
    /// * `parent` - the launching process's environment, where the forwarded variables come from.
    /// * `env_vars` - the environment the child is created with.
    fn add_mirrord_env_vars(parent: &WindowsEnv, env_vars: &mut WindowsEnv) {
        // Forward the configured variables the child's environment does not already set.
        for &env_var in FORWARDED_ENV_VARS {
            if env_vars.get(env_var).is_some() {
                continue;
            }
            if let Some(value) = parent.get(env_var) {
                env_vars.set(env_var, value.to_owned());
            } else {
                tracing::debug!("No {} found in the parent environment", env_var);
            }
        }

        // Encode and forward current socket state to child process (like Unix prepare_execve_envp)
        let encoded_sockets = match SOCKETS.lock() {
            Ok(lock) => {
                let shared_sockets = lock
                    .iter()
                    .map(|(key, value)| (*key, value))
                    .collect::<Vec<_>>();
                let socket_count = shared_sockets.len();
                let encoded = bincode::encode_to_vec(shared_sockets, bincode::config::standard())
                    .map(|bytes| BASE64_URL_SAFE.encode(bytes));
                drop(lock);

                match encoded {
                    Ok(encoded_sockets) => Some((encoded_sockets, socket_count)),
                    Err(error) => {
                        tracing::warn!("Failed to encode shared sockets: {}", error);
                        None
                    }
                }
            }
            Err(error) => {
                tracing::warn!("Failed to lock shared sockets: {}", error);
                None
            }
        };

        if let Some((encoded_sockets, socket_count)) = encoded_sockets {
            tracing::debug!(
                "Encoded and forwarding {} shared sockets to child process: {}",
                socket_count,
                encoded_sockets
            );
            env_vars.set(SHARED_SOCKETS_ENV_VAR, encoded_sockets);
        } else if let Some(existing_sockets) = parent.get(SHARED_SOCKETS_ENV_VAR) {
            tracing::debug!(
                "Fallback: forwarding existing shared sockets: {}",
                existing_sockets
            );
            env_vars.set(SHARED_SOCKETS_ENV_VAR, existing_sockets.to_owned());
        }

        // Add resolved config for child process inheritance
        // Only add if not already present in the environment we're building
        if env_vars.get(LayerConfig::RESOLVED_CONFIG_ENV).is_none() {
            // First try to get from current environment variable, then fallback to encoding current
            // config
            if let Some(resolved_config) = parent.get(LayerConfig::RESOLVED_CONFIG_ENV) {
                env_vars.set(LayerConfig::RESOLVED_CONFIG_ENV, resolved_config.to_owned());
            } else {
                // Fallback: try to encode current config if layer setup is available
                // Use a safe approach that doesn't panic if setup isn't initialized
                match std::panic::catch_unwind(|| setup().layer_config().encode()) {
                    Ok(Ok(encoded_config)) => {
                        env_vars.set(LayerConfig::RESOLVED_CONFIG_ENV, encoded_config);
                        tracing::debug!(
                            "Fallback: encoded current config for child process inheritance"
                        );
                    }
                    Ok(Err(encode_error)) => {
                        tracing::warn!(
                            "Could not encode current config for child process: {}",
                            encode_error
                        );
                    }
                    Err(_) => {
                        tracing::error!(
                            "Layer setup not available yet, cannot provide fallback config to child process"
                        );
                    }
                }
            }
        }

        // Add Windows-specific child process inheritance variables if proxy connection exists
        #[allow(static_mut_refs)]
        unsafe {
            if let Some(proxy_conn) = PROXY_CONNECTION.get() {
                // Pass current process ID as parent PID for child
                env_vars.set(
                    MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID,
                    std::process::id().to_string(),
                );

                // Pass current layer ID for child inheritance
                env_vars.set(
                    MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID,
                    proxy_conn.layer_id().0.to_string(),
                );

                // Pass proxy address for child connection
                env_vars.set(
                    MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR,
                    proxy_conn.proxy_addr().to_string(),
                );
            }
        }
    }

    /// Creates an anonymous job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and
    /// assigns `process` to it, returning the job handle.
    ///
    /// While the returned handle is held open the job's processes run; when the last
    /// handle to the job closes — including when *this* process dies and the OS
    /// reclaims its handles — the OS terminates every process still in the job. Assign
    /// the child while it is suspended so it is bound before it can spawn descendants.
    fn assign_to_kill_on_close_job(process: HANDLE) -> LayerResult<HANDLE> {
        unsafe {
            let job = CreateJobObjectW(ptr::null_mut(), ptr::null());
            if job.is_null() {
                return Err(LayerError::WindowsProcessCreation(
                    WindowsError::last_error(),
                ));
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as LPVOID,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as DWORD,
            ) == 0
            {
                let error = WindowsError::last_error();
                CloseHandle(job);
                return Err(LayerError::WindowsProcessCreation(error));
            }

            if AssignProcessToJobObject(job, process) == 0 {
                let error = WindowsError::last_error();
                CloseHandle(job);
                return Err(LayerError::WindowsProcessCreation(error));
            }

            Ok(job)
        }
    }

    /// Execute process with layer injection for CLI context, returning managed process.
    ///
    /// A process mirrord cannot load its layer into is ended and reported as an error (see
    /// [`LayerFailurePolicy::Terminate`]): the user asked for this program under mirrord, and
    /// nothing retries it.
    ///
    /// `progress` succeeds before the program's primary thread first runs, so a caller that shares
    /// its console with the program can end its own output there.
    pub fn execute<P>(
        application_name: Option<String>,
        command_line: String,
        current_directory: Option<String>,
        env_vars: WindowsEnv,
        injection_method: InjectionMethod,
        kill_children_on_exit: bool,
        progress: Option<P>,
    ) -> LayerResult<Self>
    where
        P: mirrord_progress::Progress,
    {
        // For CLI context, create default parameters and use execute_with_closure
        let default_creation_flags = 0;
        // The CLI hands the child its own console handles, so the program's output reaches the
        // user's terminal.
        let mut default_startup_info = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: Self::ensure_handle_inheritable(unsafe { GetStdHandle(STD_INPUT_HANDLE) })?,
            hStdOutput: Self::ensure_handle_inheritable(unsafe {
                GetStdHandle(STD_OUTPUT_HANDLE)
            })?,
            hStdError: Self::ensure_handle_inheritable(unsafe { GetStdHandle(STD_ERROR_HANDLE) })?,
            ..unsafe { std::mem::zeroed() }
        };

        let create_process_fn = |creation_flags, environment, startup_info: &mut STARTUPINFOW| unsafe {
            // Convert strings to wide character format for Windows API
            let app_name_wide = if let Some(ref name) = application_name {
                string_to_u16_buffer(name)
            } else {
                vec![0]
            };
            let mut command_line_wide = string_to_u16_buffer(&command_line);
            let current_dir_wide = if let Some(ref dir) = current_directory {
                string_to_u16_buffer(dir)
            } else {
                vec![0]
            };

            let mut process_info: PROCESS_INFORMATION = std::mem::zeroed();

            let success = CreateProcessW(
                if application_name.is_some() {
                    app_name_wide.as_ptr()
                } else {
                    ptr::null()
                },
                command_line_wide.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                // Enable handle inheritance so child can inherit console handles
                true.into(),
                creation_flags,
                environment,
                if current_directory.is_some() {
                    current_dir_wide.as_ptr()
                } else {
                    ptr::null()
                },
                startup_info,
                &mut process_info,
            );

            if success != 0 {
                Ok(process_info)
            } else {
                Err(LayerError::WindowsProcessCreation(
                    WindowsError::last_error(),
                ))
            }
        };

        Self::execute_with_closure(
            &WindowsEnv::inherited(),
            Some(env_vars),
            injection_method,
            default_creation_flags,
            &mut default_startup_info,
            create_process_fn,
            kill_children_on_exit,
            LayerFailurePolicy::Terminate,
            progress,
        )
        .map_err(|launch| launch.error)
    }

    /// Execute process with layer injection using a closure for the original function call.
    /// This method is optimized for hook contexts where all original parameters are available.
    ///
    /// The caller's `STARTUPINFO` reaches `create_process_fn` untouched, standard handles
    /// included. A caller that asked for `CREATE_SUSPENDED` gets its child back suspended, with
    /// exactly the one suspension it asked for.
    ///
    /// # Errors
    ///
    /// A [`LaunchError`] says whether the process had been created. Once it had, it must not be
    /// created again: it may already have run, and its side effects would run twice.
    ///
    /// # Arguments
    ///
    /// * `parent` - the launching process's environment. The layer file and the variables Windows
    ///   needs come from it when `caller_env_vars` lacks them, and mirrord's own settings always
    ///   do.
    /// * `caller_env_vars` - the environment the creator asked for. `None` inherits `parent`; an
    ///   empty one asks for no variables at all, which is a different request.
    /// * `injection_method` - how the layer is loaded. The child's environment carries it on, so
    ///   its own descendants are loaded the same way.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_closure<F, P>(
        parent: &WindowsEnv,
        caller_env_vars: Option<WindowsEnv>,
        injection_method: InjectionMethod,
        caller_creation_flags: DWORD,
        caller_startup_info: &mut STARTUPINFOW,
        create_process_fn: F,
        kill_children_on_exit: bool,
        on_layer_failure: LayerFailurePolicy,
        progress: Option<P>,
    ) -> Result<Self, LaunchError>
    where
        F: FnOnce(DWORD, LPVOID, &mut STARTUPINFOW) -> LayerResult<PROCESS_INFORMATION>,
        P: mirrord_progress::Progress,
    {
        let not_created = |error| LaunchError {
            error,
            created: false,
        };
        let dll_path = caller_env_vars
            .as_ref()
            .and_then(|environment| environment.get(MIRRORD_LAYER_FILE_ENV))
            .or_else(|| parent.get(MIRRORD_LAYER_FILE_ENV))
            .map(str::to_owned)
            .ok_or(LayerError::VarError(std::env::VarError::NotPresent))
            .map_err(not_created)?;
        if !std::path::Path::new(&dll_path).exists() {
            return Err(not_created(LayerError::DllInjection(format!(
                "DLL file not found: {}",
                dll_path
            ))));
        }

        // An explicit environment is the creator's choice, apart from the variables Windows cannot
        // run without, which come from the parent when the creator left them out. Either way the
        // child gets mirrord's variables on top.
        let mut environment = match caller_env_vars {
            Some(mut environment) => {
                for &name in SYSTEM_VARIABLES {
                    if environment.get(name).is_none()
                        && let Some(value) = parent.get(name)
                    {
                        environment.set(name, value.to_owned());
                    }
                }
                environment
            }
            None => parent.clone(),
        };
        Self::add_mirrord_env_vars(parent, &mut environment);
        environment.set(MIRRORD_INJECTION_METHOD_ENV, injection_method.to_string());
        let mut env_storage = environment.to_block();
        let environment_ptr = env_storage.as_mut_ptr() as LPVOID;
        let reporting = StartupReporting::from_environment(&environment);
        let debugger_setting = environment
            .get(MIRRORD_LAYER_WAIT_FOR_DEBUGGER)
            .map(str::to_owned);

        // Calculate final creation flags (original + environment + suspended)
        let creation_flags = caller_creation_flags | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;
        let caller_suspended = caller_creation_flags & CREATE_SUSPENDED != 0;

        // Call the original function with processed parameters
        let process_info = create_process_fn(creation_flags, environment_ptr, caller_startup_info)
            .map_err(not_created)?;

        // Take ownership immediately so every later error path terminates the suspended process
        // and closes both process handles.
        let mut managed_process = Self {
            process_info,
            released: false,
            terminate_on_drop: true,
            job: None,
        };

        // Bind the child's lifetime to ours while it is still suspended, so an abrupt
        // kill of this process can't orphan it (see [`LayerManagedProcess::job`]). Some
        // hosts run mirrord inside a restrictive job that rejects nested assignment; in
        // that case preserve the pre-existing process behavior and fall back to Drop-based
        // cleanup instead of making an otherwise valid launch fail.
        if kill_children_on_exit {
            match Self::assign_to_kill_on_close_job(managed_process.process_info.hProcess) {
                Ok(job) => managed_process.job = Some(job),
                Err(error) => {
                    tracing::warn!(%error, "failed to bind child process to a kill-on-close job");
                }
            }
        }

        // Matched against the image that was actually created, the way the child's layer matches
        // it, so both sides agree on whether the layer waits.
        let debugger_wait = debugger_setting.as_deref().is_some_and(|setting| {
            let image = utils_win::process::process_status(process_info.dwProcessId).name;
            let image_stem = Path::new(&image)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            debugger_wait_targets(setting, &image_stem)
        });

        let plan = LaunchPlan {
            dll_path: &dll_path,
            injection_method,
            caller_suspended,
            on_layer_failure,
            reporting,
            debugger_wait,
        };
        managed_process
            .inject_and_resume(&plan, progress)
            .map_err(|error| LaunchError {
                error,
                created: true,
            })
    }

    /// Loads the layer into the created, still-suspended process, waits for it where that is
    /// possible, and resumes the process unless its creator asked to keep it suspended.
    ///
    /// Everything here happens after creation, so no failure may lead anyone to create the
    /// process again. What a failure means for the process follows `plan.on_layer_failure`; a
    /// failure it cannot survive (an injection that left the image unsafe to run, a resume that
    /// failed) returns the error under either policy, and dropping `self` ends the process. Every
    /// failure is reported to the session's crash monitor, under either policy, so the session
    /// keeps the files the report points at.
    ///
    /// The primary thread is resumed at most once here, and never when the creator asked for
    /// `CREATE_SUSPENDED`: that one suspension is the creator's to lift.
    fn inject_and_resume<P>(
        self,
        plan: &LaunchPlan<'_>,
        mut progress: Option<P>,
    ) -> LayerResult<Self>
    where
        P: mirrord_progress::Progress,
    {
        let child_pid = self.process_info.dwProcessId;
        let method = plan.injection_method;

        // The child is suspended, so we can create the PID-based event before injection.
        let parent_events = match ParentInitEvents::create(child_pid) {
            Ok(parent_events) => parent_events,
            Err(error) => {
                tracing::error!(child_pid, %error, "inject: could not create the readiness events");
                let reason = format!(
                    "mirrord could not create the events its layer reports readiness through \
                     ({error}), so it did not load the layer into this process."
                );
                return self.without_layer(plan, &reason, error);
            }
        };

        // These handles come directly from CREATE_SUSPENDED and remain owned by self.
        let target = BorrowedTarget::new(self.process_handle())?
            .with_main_thread(unsafe {
                BorrowedHandle::borrow_raw(self.process_info.hThread.cast())
            })
            .with_loader_state(LoaderState::NotStarted);
        tracing::debug!(
            child_pid,
            %method,
            caller_suspended = plan.caller_suspended,
            debugger_wait = plan.debugger_wait,
            "inject: begin"
        );
        let injector = injector_for(method, plan.debugger_wait);
        let injected = match unsafe { injector.inject(&target, plan.dll_path) } {
            Ok(injected) => injected,
            Err(error) if error.must_not_resume() => {
                // The image was changed and could not be put back, so no thread of it may run.
                tracing::error!(child_pid, %method, %error, "inject: failed, and the process must not run");
                self.report(
                    plan,
                    InitReport::Failed(format!(
                        "Loading the layer failed in a way that left this process unsafe to run \
                         ({error}), so mirrord ended it before it started."
                    )),
                );
                return Err(error.into());
            }
            Err(error) if error.is_pending() => {
                // The remote load is still running, so the layer may still come up: this is not a
                // process without mirrord, and it must not be loaded a second time.
                tracing::error!(child_pid, %method, %error, "inject: the layer's load did not finish in time");
                let reason = format!(
                    "Loading the layer into this process did not finish in time ({error}). It may \
                     still come up, or the process may stall on its first remote call. {}",
                    plan.reporting.log_hint()
                );
                return self.settle_failure(plan, LoadTiming::Immediate, &reason, error.into());
            }
            Err(error) => {
                tracing::error!(child_pid, %method, %error, "inject: failed");
                let reason = format!(
                    "mirrord could not load its layer into this process with {method} injection \
                     ({error})."
                );
                return self.without_layer(plan, &reason, error.into());
            }
        };

        match injected.timing {
            // The layer loads when the primary thread first runs, and that is the creator's call
            // to make. Nobody waits here, so both events have to live in the child until its
            // `DllMain` opens them. Whether the layer then comes up is in the child's layer log.
            LoadTiming::OnResume if plan.caller_suspended => {
                match parent_events.keep_alive_in_process(self.process_handle()) {
                    Ok(remote) => remote.retain(),
                    Err(error) => tracing::warn!(
                        child_pid,
                        %error,
                        "inject: the readiness events could not be kept alive in the child, so its layer reports to nobody"
                    ),
                }
                tracing::debug!(
                    child_pid,
                    "inject: layer loading queued until the creator resumes the process"
                );
                if let Some(progress) = progress.as_mut() {
                    progress.success(Some("layer loading queued"));
                }
                return Ok(self);
            }
            // The program runs from here on, while its layer comes up, and it shares the
            // launcher's console: progress is done before the program can write to it, and the
            // wait below only decides how the launch ends.
            LoadTiming::OnResume => {
                if let Some(mut progress) = progress.take() {
                    progress.success(Some("layer loading on resume"));
                }
                self.resume_main_thread()?;
            }
            LoadTiming::Immediate => {}
        }

        // A layer that waits for a debugger takes as long as attaching one does.
        let timeout_ms = (!plan.debugger_wait).then_some(LAYER_INIT_TIMEOUT_MS);
        tracing::debug!(child_pid, ?timeout_ms, "wait: begin");

        match parent_events.wait(self.process_handle(), timeout_ms) {
            Ok(InitWaitOutcome::Signaled) => {
                tracing::debug!(child_pid, "wait: signaled");
                if let Some(progress) = progress.as_mut() {
                    progress.success(Some("Ready!"));
                }
            }
            Ok(InitWaitOutcome::Failed) => {
                // Signaled by a layer that gave up, inside `DllMain` (it stays loaded but inert,
                // with no hook enabled) or on its startup worker (the process then ends). The
                // child reports the second kind itself as well; the monitor joins the two.
                tracing::error!(
                    child_pid,
                    %method,
                    "wait: the layer reported that it could not initialize. Its layer log names the cause"
                );
                let reason = format!(
                    "The layer reported that it could not initialize. {}",
                    plan.reporting.log_hint()
                );
                let error = LayerError::ProcessSynchronization(format!(
                    "the mirrord layer failed to initialize in process {child_pid}. {}",
                    plan.reporting.log_hint()
                ));
                return self.settle_failure(plan, injected.timing, &reason, error);
            }
            Ok(InitWaitOutcome::ProcessExited) => {
                // The process is gone either way, so there is nothing to resume and nothing to
                // create again.
                let exit_code = self.exit_code();
                // A program shorter-lived than the layer's async startup never gets to send the
                // signal, and that is ordinary. One whose primary thread never ran (the layer
                // loaded before it, and nothing resumed it yet) did not get to run its program at
                // all, and a loader exit code says the loader gave up.
                if injected.timing != LoadTiming::Immediate
                    && exit_code.and_then(loader_status_name).is_none()
                {
                    tracing::debug!(
                        child_pid,
                        ?exit_code,
                        "wait: the process exited before the layer reported ready"
                    );
                    return Ok(self);
                }
                let exit = exit_code.map_or_else(
                    || "an exit code that could not be read".to_owned(),
                    |code| match loader_status_name(code) {
                        Some(name) => format!("{code:#010x} ({name})"),
                        None => format!("{code:#010x}"),
                    },
                );
                tracing::error!(child_pid, %method, %exit, "wait: the process exited before it could run its program");
                let reason = format!(
                    "This process exited with {exit} before it ran its program, while mirrord's \
                     layer was loading. A loader error usually means Windows refused a module, \
                     the layer or one it depends on (a code-integrity policy, say); otherwise the \
                     layer's startup, another DLL's initializer or an outside kill ended it. {}",
                    plan.reporting.log_hint()
                );
                self.report(plan, InitReport::Failed(reason));
                return match plan.on_layer_failure {
                    LayerFailurePolicy::Terminate => {
                        Err(LayerError::ProcessSynchronization(format!(
                            "process {child_pid} exited with {exit} while the mirrord layer was \
                             loading. {}",
                            plan.reporting.log_hint()
                        )))
                    }
                    LayerFailurePolicy::RunWithoutMirrord => Ok(self),
                };
            }
            Ok(InitWaitOutcome::TimedOut) => {
                // The layer is loaded and its hooks are live; it is only late. Nothing says it
                // failed, so under either policy that is a note, not a crash: no dialog, and the
                // session is not marked as crashed. Under `exec` the launch error says the rest.
                tracing::warn!(
                    child_pid,
                    %method,
                    "wait: the layer did not report ready within {LAYER_INIT_TIMEOUT_MS} ms"
                );
                let what = format!(
                    "the layer loaded, but did not report ready within {LAYER_INIT_TIMEOUT_MS} ms. \
                     {}",
                    plan.reporting.log_hint()
                );
                match plan.on_layer_failure {
                    LayerFailurePolicy::Terminate => {
                        self.report(
                            plan,
                            InitReport::SlowStart(format!(
                                "In this process {what} mirrord ended it."
                            )),
                        );
                        return Err(LayerError::ProcessSynchronization(format!(
                            "in process {child_pid} {what}"
                        )));
                    }
                    LayerFailurePolicy::RunWithoutMirrord => self.report(
                        plan,
                        InitReport::SlowStart(format!(
                            "In this process {what} The process keeps running, and its hooked \
                             calls wait for the layer."
                        )),
                    ),
                }
            }
            Err(error) => {
                tracing::error!(child_pid, %method, %error, "wait: waiting for the layer failed");
                let reason = format!(
                    "The layer loaded into this process, but mirrord could not wait for it to \
                     report ready ({error}). It may still come up. {}",
                    plan.reporting.log_hint()
                );
                return self.settle_failure(plan, injected.timing, &reason, error);
            }
        }

        // Readiness and a slow start end the same way: the process was never resumed (unless
        // loading needed it), and now it is, once, unless its creator keeps it suspended.
        self.resume_after_wait(plan, injected.timing)?;
        Ok(self)
    }

    /// Settles a failure that happened after the layer was, or may have been, loaded.
    ///
    /// Reports `reason` under either policy. [`LayerFailurePolicy::Terminate`] returns `error`,
    /// and dropping `self` ends the process. [`LayerFailurePolicy::RunWithoutMirrord`] keeps it,
    /// resuming it if nothing did yet.
    fn settle_failure(
        self,
        plan: &LaunchPlan<'_>,
        timing: LoadTiming,
        reason: &str,
        error: LayerError,
    ) -> LayerResult<Self> {
        self.report(plan, InitReport::Failed(reason.to_owned()));
        match plan.on_layer_failure {
            LayerFailurePolicy::Terminate => Err(error),
            LayerFailurePolicy::RunWithoutMirrord => {
                self.resume_after_wait(plan, timing)?;
                Ok(self)
            }
        }
    }

    /// Resumes a process whose layer loaded immediately, once its wait is over.
    ///
    /// A process whose layer loads on resume was resumed before the wait, and one whose creator
    /// asked for `CREATE_SUSPENDED` is the creator's to resume.
    fn resume_after_wait(&self, plan: &LaunchPlan<'_>, timing: LoadTiming) -> LayerResult<()> {
        if timing == LoadTiming::Immediate && !plan.caller_suspended {
            self.resume_main_thread()?;
        }
        Ok(())
    }

    /// Settles a process mirrord could not load its layer into, before its primary thread ever
    /// ran.
    ///
    /// Reports under either policy. [`LayerFailurePolicy::Terminate`] returns `error`, and
    /// dropping `self` ends the process. [`LayerFailurePolicy::RunWithoutMirrord`] resumes the
    /// process, unless its creator keeps it suspended.
    fn without_layer(
        self,
        plan: &LaunchPlan<'_>,
        reason: &str,
        error: LayerError,
    ) -> LayerResult<Self> {
        match plan.on_layer_failure {
            LayerFailurePolicy::Terminate => {
                self.report(
                    plan,
                    InitReport::Failed(format!("{reason} mirrord ended the process.")),
                );
                Err(error)
            }
            LayerFailurePolicy::RunWithoutMirrord => {
                self.report(
                    plan,
                    InitReport::Failed(format!("{reason} The process runs without mirrord.")),
                );
                if !plan.caller_suspended {
                    self.resume_main_thread()?;
                }
                Ok(self)
            }
        }
    }

    /// Reports a startup problem of this process to its session's crash monitor, when it has one.
    fn report(&self, plan: &LaunchPlan<'_>, report: InitReport) {
        let Some(monitor) = plan.reporting.monitor else {
            tracing::debug!(
                child_pid = self.process_info.dwProcessId,
                "no crash monitor in the child's environment, so its startup problem is only logged"
            );
            return;
        };
        utils_win::diagnostics::monitor::report_init_failure_for(
            monitor,
            self.process_info.dwProcessId,
            std::process::id(),
            report,
        );
    }

    /// The process handle, borrowed from `self`.
    fn process_handle(&self) -> BorrowedHandle<'_> {
        // SAFETY: `self` owns the handle and closes it only on drop.
        unsafe { BorrowedHandle::borrow_raw(self.process_info.hProcess.cast()) }
    }

    /// The exit code of a process that has exited, when it can be read.
    fn exit_code(&self) -> Option<u32> {
        let mut code = 0;
        (unsafe { GetExitCodeProcess(self.process_info.hProcess, &mut code) } != 0).then_some(code)
    }

    /// Lifts the one suspension this launcher added.
    ///
    /// A process that already ended has nothing to resume, and that is not an error: whatever
    /// ended it (its own startup, another process) is the outcome the caller gets.
    fn resume_main_thread(&self) -> LayerResult<()> {
        let previous = unsafe { ResumeThread(self.process_info.hThread) };
        // A process being ended stops answering for its threads before it is signaled, so an
        // unexpected answer gets a moment to turn into an exit.
        if previous != 1 && self.exits_within(EXIT_GRACE_MS) {
            tracing::debug!(
                child_pid = self.process_info.dwProcessId,
                "the process ended before its primary thread was resumed"
            );
            return Ok(());
        }
        if previous == u32::MAX {
            return Err(LayerError::WindowsProcessCreation(
                WindowsError::last_error(),
            ));
        }
        if previous != 1 {
            return Err(LayerError::ProcessSynchronization(format!(
                "expected one suspension before resume, got {previous}"
            )));
        }
        Ok(())
    }

    /// Whether the process ends within `timeout_ms`.
    fn exits_within(&self, timeout_ms: u32) -> bool {
        unsafe { WaitForSingleObject(self.process_info.hProcess, timeout_ms) == WAIT_OBJECT_0 }
    }

    /// Release process from management (won't be terminated on drop)
    pub fn release(mut self) -> PROCESS_INFORMATION {
        assert!(
            self.job.is_none(),
            "a process assigned to a kill-on-close job cannot be released"
        );
        self.released = true;
        self.process_info
    }

    /// Wait for process to exit and return exit code
    pub fn wait_until_exit(self) -> LayerResult<u32> {
        let exit_code = unsafe {
            let wait_result = WaitForSingleObject(self.process_info.hProcess, INFINITE);
            if wait_result != WAIT_OBJECT_0 {
                return Err(LayerError::WindowsProcessCreation(
                    WindowsError::last_error(),
                ));
            }

            let mut exit_code = 0u32;
            if GetExitCodeProcess(self.process_info.hProcess, &mut exit_code) == 0 {
                return Err(LayerError::WindowsProcessCreation(
                    WindowsError::last_error(),
                ));
            }
            exit_code
        };

        Ok(exit_code)
    }
}

impl Deref for LayerManagedProcess {
    type Target = PROCESS_INFORMATION;

    fn deref(&self) -> &Self::Target {
        &self.process_info
    }
}

impl Drop for LayerManagedProcess {
    fn drop(&mut self) {
        if self.released {
            return;
        }

        if self.terminate_on_drop {
            unsafe {
                TerminateProcess(self.process_info.hProcess, 1);
            }
        }
        unsafe {
            CloseHandle(self.process_info.hProcess);
            CloseHandle(self.process_info.hThread);
        }

        // Closing the last job handle reaps any child still in the job (KILL_ON_JOB_CLOSE).
        if let Some(job) = self.job.take() {
            unsafe {
                CloseHandle(job);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::CString,
        io::{ErrorKind, Write},
        net::TcpListener,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        sync::{Arc, Mutex},
        thread::JoinHandle,
        time::{Duration, Instant},
    };

    use utils_win::diagnostics::monitor::{ACK_READY, Registration, read_registration};
    use winapi::{
        shared::minwindef::FALSE,
        um::{
            handleapi::DuplicateHandle,
            processthreadsapi::{GetCurrentProcess, OpenProcess},
            synchapi::{OpenEventA, SetEvent},
            winnt::{DUPLICATE_SAME_ACCESS, EVENT_MODIFY_STATE, PROCESS_TERMINATE, SYNCHRONIZE},
        },
    };

    use super::*;

    /// The exit code the test program ends with, so an exit with it proves the program ran.
    const RAN: u32 = 7;

    /// What the fake layer does once the child exists.
    #[derive(Clone, Copy)]
    enum Layer {
        /// Signals readiness, as a layer whose startup succeeded.
        Ready,
        /// Signals readiness only after the launcher's usual timeout, as a layer that waited for a
        /// debugger.
        LateReady,
        /// Signals failure, as a layer whose `DllMain` gave up.
        Failed,
        /// Ends the child, as a layer whose startup ended the process.
        Kills,
        /// Does nothing: for paths that never wait, and for a layer that never reports.
        Silent,
    }

    /// A program that outlives the readiness timeout, then ends with [`RAN`].
    const OUTLIVES_THE_WAIT: &str = "ping -n 4 127.0.0.1 > nul & exit 7";

    /// A launch of `cmd.exe /c exit 7` with a real injection of a harmless system DLL, and a
    /// thread standing in for the layer that signals the child's events.
    struct Launch {
        method: InjectionMethod,
        payload: String,
        caller_suspended: bool,
        policy: LayerFailurePolicy,
        layer: Layer,
        /// What `cmd.exe /c` runs.
        command: &'static str,
        /// `MIRRORD_LAYER_WAIT_FOR_DEBUGGER` in the child's environment.
        wait_for_debugger: Option<&'static str>,
        /// The environment the creator asks for, before the launcher adds to it.
        environment: WindowsEnv,
        /// The launching process's environment, which the launcher forwards from.
        parent: WindowsEnv,
    }

    struct Launched {
        result: Result<LayerManagedProcess, LaunchError>,
        /// A handle of the test's own, duplicated at creation, so the child's exit code can be
        /// read after the launcher closed its handles.
        process: OwnedHandle,
        creations: u32,
        /// What the launcher reported as done, which tells readiness apart from a fallback.
        progress: Vec<String>,
        /// The environment block the launcher created the child with.
        child_environment: WindowsEnv,
    }

    impl Launched {
        /// The child's exit code, once it has exited.
        fn exit_code(&self) -> u32 {
            let process = self.process.as_raw_handle().cast();
            let mut code = 0;
            unsafe {
                assert_eq!(
                    WaitForSingleObject(process, 10_000),
                    WAIT_OBJECT_0,
                    "the child exits"
                );
                assert_ne!(
                    GetExitCodeProcess(process, &mut code),
                    0,
                    "read the exit code"
                );
            }
            code
        }
    }

    /// Keeps what the launcher reports, so a test can tell which path it took.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl mirrord_progress::Progress for Recorder {
        fn subtask(&self, _: &str) -> Self {
            self.clone()
        }

        fn success(&mut self, message: Option<&str>) {
            self.0
                .lock()
                .expect("progress")
                .push(message.unwrap_or_default().to_owned());
        }
    }

    fn system_dll(name: &str) -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        format!(r"{root}\System32\{name}")
    }

    impl Launch {
        fn new(method: InjectionMethod, layer: Layer) -> Self {
            Self {
                method,
                payload: system_dll("version.dll"),
                caller_suspended: false,
                policy: LayerFailurePolicy::RunWithoutMirrord,
                layer,
                command: "exit 7",
                wait_for_debugger: None,
                environment: WindowsEnv::from_ordered_entries(
                    ["SystemRoot", "PATH"]
                        .into_iter()
                        .filter_map(|name| Some((name.to_owned(), std::env::var(name).ok()?))),
                ),
                parent: WindowsEnv::inherited(),
            }
        }

        fn run(self) -> Launched {
            let mut environment = self.environment;
            environment.set(MIRRORD_LAYER_FILE_ENV, self.payload.clone());
            // Keeps `add_mirrord_env_vars` from reaching for a layer setup these tests do not have.
            environment.set(LayerConfig::RESOLVED_CONFIG_ENV, String::new());
            if let Some(setting) = self.wait_for_debugger {
                environment.set(MIRRORD_LAYER_WAIT_FOR_DEBUGGER, setting.to_owned());
            }

            let child_pid = Arc::new(Mutex::new(None::<u32>));
            let fake_layer = fake_layer(self.layer, Arc::clone(&child_pid));

            let mut creations = 0;
            let mut duplicate = None;
            let mut child_environment = None;
            let mut startup_info = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                ..unsafe { std::mem::zeroed() }
            };
            let create = |flags: DWORD, environment: LPVOID, startup_info: &mut STARTUPINFOW| {
                creations += 1;
                child_environment =
                    Some(unsafe { WindowsEnv::from_block::<u16>(environment.cast()) });
                let application = string_to_u16_buffer(system_dll("cmd.exe"));
                let mut command_line = string_to_u16_buffer(format!("cmd.exe /c {}", self.command));
                let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
                let created = unsafe {
                    CreateProcessW(
                        application.as_ptr(),
                        command_line.as_mut_ptr(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        FALSE,
                        flags,
                        environment,
                        ptr::null(),
                        startup_info,
                        &mut process_info,
                    )
                };
                if created == 0 {
                    return Err(LayerError::WindowsProcessCreation(
                        WindowsError::last_error(),
                    ));
                }
                let mut own = ptr::null_mut();
                let duplicated = unsafe {
                    DuplicateHandle(
                        GetCurrentProcess(),
                        process_info.hProcess,
                        GetCurrentProcess(),
                        &mut own,
                        0,
                        FALSE,
                        DUPLICATE_SAME_ACCESS,
                    )
                };
                assert_ne!(duplicated, 0, "duplicate the child's handle");
                duplicate = Some(unsafe { OwnedHandle::from_raw_handle(own.cast()) });
                *child_pid.lock().expect("pid") = Some(process_info.dwProcessId);
                Ok(process_info)
            };

            let creation_flags = if self.caller_suspended {
                CREATE_SUSPENDED
            } else {
                0
            };
            let recorder = Recorder::default();
            let result = LayerManagedProcess::execute_with_closure(
                &self.parent,
                Some(environment),
                self.method,
                creation_flags,
                &mut startup_info,
                create,
                false,
                self.policy,
                Some(recorder.clone()),
            );
            fake_layer.join().expect("fake layer");
            let progress = recorder.0.lock().expect("progress").clone();

            Launched {
                result,
                process: duplicate.expect("the child was created"),
                creations,
                progress,
                child_environment: child_environment.expect("the child was created"),
            }
        }
    }

    /// Plays the layer's part: waits for the child and its events, then does what `layer` says.
    fn fake_layer(layer: Layer, child_pid: Arc<Mutex<Option<u32>>>) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let pid = loop {
                if let Some(pid) = *child_pid.lock().expect("pid") {
                    break pid;
                }
                assert!(Instant::now() < deadline, "the child was never created");
                std::thread::sleep(Duration::from_millis(5));
            };

            let name = match layer {
                Layer::Failed => format!("mirrord_layer_init_failed_{pid}"),
                Layer::Ready | Layer::LateReady | Layer::Kills => {
                    format!("mirrord_layer_init_{pid}")
                }
                Layer::Silent => return,
            };

            let name = CString::new(name).expect("name");
            let event = loop {
                let event = unsafe { OpenEventA(EVENT_MODIFY_STATE, FALSE, name.as_ptr()) };
                if !event.is_null() {
                    break event;
                }
                assert!(
                    Instant::now() < deadline,
                    "the parent never created its events"
                );
                std::thread::sleep(Duration::from_millis(5));
            };

            if let Layer::Kills = layer {
                // Whether this lands during the injection or during the wait, the process is gone
                // before anything resumed it.
                let process = unsafe { OpenProcess(PROCESS_TERMINATE, FALSE, pid) };
                assert!(!process.is_null(), "open the child to end it");
                unsafe {
                    TerminateProcess(process, 42);
                    CloseHandle(process);
                    CloseHandle(event);
                }
                return;
            }

            if let Layer::LateReady = layer {
                std::thread::sleep(Duration::from_millis(u64::from(LAYER_INIT_TIMEOUT_MS) * 2));
            }

            unsafe {
                SetEvent(event);
                CloseHandle(event);
            }
        })
    }

    /// An explicit environment, even one without `SystemRoot`, gets what Windows needs to start
    /// Winsock, and nothing else of this process's environment. The creator's own value, and its
    /// casing, are kept. The child's environment also names the injection method, so its own
    /// children are loaded the same way.
    #[test]
    fn an_explicit_environment_gets_only_the_system_variables() {
        let system_root = std::env::var("SystemRoot").expect("this process has a SystemRoot");
        let launched = Launch {
            environment: WindowsEnv::new(),
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::Ready)
        }
        .run();
        assert_eq!(launched.exit_code(), RAN);
        let child = &launched.child_environment;
        assert_eq!(child.get("SystemRoot"), Some(system_root.as_str()));
        assert_eq!(child.get("PATH"), None, "this process's PATH stays out");
        assert_eq!(
            child.get(MIRRORD_INJECTION_METHOD_ENV),
            Some(InjectionMethod::LoadLibrary.to_string().as_str())
        );

        let launched = Launch {
            environment: WindowsEnv::from_ordered_entries([(
                "SYSTEMROOT".to_owned(),
                system_root.clone(),
            )]),
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::Ready)
        }
        .run();
        assert_eq!(launched.exit_code(), RAN);
        assert!(
            launched
                .child_environment
                .iter()
                .any(|entry| entry == ("SYSTEMROOT", system_root.as_str())),
            "the creator's casing is kept"
        );
    }

    /// A child given an explicit environment still gets what its layer's setup reads from the
    /// parent's environment.
    #[test]
    fn layer_settings_are_forwarded_to_child_processes() {
        let settings = [
            (MIRRORD_LAYER_INTPROXY_ADDR, "127.0.0.1:1"),
            (MIRRORD_LAYER_TARGET_CONTAINER_PORTS, "8080"),
            (MIRRORD_FS_PREFETCH_DIR, r"C:\mirrord-forwarding-test"),
            (MIRRORD_LAYER_CRASH_REPORTING, "false"),
        ];
        let mut launch = Launch::new(InjectionMethod::LoadLibrary, Layer::Ready);
        for (name, value) in settings {
            launch.parent.set(name, value.to_owned());
        }
        let launched = launch.run();

        assert_eq!(launched.exit_code(), RAN);
        for (name, value) in settings {
            assert_eq!(
                launched.child_environment.get(name),
                Some(value),
                "{name} is forwarded"
            );
        }
    }

    /// Under `mirrord exec` the child's environment is the session's, and its values must win
    /// over the CLI's own: a relative log directory the CLI made absolute, or the proxy of an
    /// outer session in a nested `mirrord exec`.
    #[test]
    fn the_childs_own_settings_win_over_the_parents() {
        let mut launch = Launch::new(InjectionMethod::LoadLibrary, Layer::Ready);
        launch.parent.set(MIRRORD_LAYER_LOG_PATH, "logs".to_owned());
        launch
            .parent
            .set(MIRRORD_LAYER_INTPROXY_ADDR, "127.0.0.1:1".to_owned());
        launch
            .environment
            .set(MIRRORD_LAYER_LOG_PATH, r"C:\session-logs".to_owned());
        let launched = launch.run();

        assert_eq!(launched.exit_code(), RAN);
        assert_eq!(
            launched.child_environment.get(MIRRORD_LAYER_LOG_PATH),
            Some(r"C:\session-logs"),
            "the session's log directory reaches the child"
        );
        assert_eq!(
            launched.child_environment.get(MIRRORD_LAYER_INTPROXY_ADDR),
            Some("127.0.0.1:1"),
            "a setting the child's environment leaves out still comes from the parent"
        );
    }

    /// Resumes a child left suspended for its creator.
    ///
    /// # Returns
    ///
    /// The suspension count before the resume.
    fn resume(process: &LayerManagedProcess) -> u32 {
        unsafe { ResumeThread(process.hThread) }
    }

    #[test]
    fn a_ready_layer_resumes_the_program_once() {
        // A layer that loads on resume finishes progress before the program runs, not on ready.
        for (method, reported) in [
            (InjectionMethod::LoadLibrary, "Ready!"),
            (InjectionMethod::Apc, "layer loading on resume"),
        ] {
            let launched = Launch::new(method, Layer::Ready).run();
            assert_eq!(
                launched.progress,
                [reported],
                "{method}: the layer was injected"
            );
            assert_eq!(launched.creations, 1);
            assert!(launched.result.is_ok(), "{method}: launch");
            assert_eq!(launched.exit_code(), RAN, "{method}");
        }
    }

    #[test]
    fn a_failed_layer_runs_the_program_once_without_mirrord() {
        for method in [InjectionMethod::LoadLibrary, InjectionMethod::Apc] {
            let launched = Launch::new(method, Layer::Failed).run();
            assert!(
                launched.result.is_ok(),
                "a failed layer is not a failed launch"
            );
            assert_eq!(launched.creations, 1);
            assert_eq!(launched.exit_code(), RAN, "{method}");
        }
    }

    #[test]
    fn a_failed_layer_ends_the_program_when_the_launcher_says_so() {
        let launched = Launch {
            policy: LayerFailurePolicy::Terminate,
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::Failed)
        }
        .run();
        assert_eq!(launched.creations, 1);
        let error = launched.result.as_ref().err().expect("the launch fails");
        assert!(
            error.created,
            "the process existed, so it must not be created again"
        );
        assert!(
            error.to_string().contains("failed to initialize"),
            "the layer's failure, not the injection's: {error}"
        );
        assert_ne!(launched.exit_code(), RAN, "the program never ran");
    }

    /// A process whose primary thread never ran exited while the layer was loading. Under the
    /// hook it is handed back as it is, and under `exec` it is an error, not an exit code.
    #[test]
    fn a_process_that_exits_before_resume_is_reported() {
        let launched = Launch::new(InjectionMethod::LoadLibrary, Layer::Kills).run();
        assert_eq!(launched.creations, 1);
        assert!(launched.result.is_ok(), "an exit is not a failed launch");
        assert_eq!(launched.exit_code(), 42);

        let launched = Launch {
            policy: LayerFailurePolicy::Terminate,
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::Kills)
        }
        .run();
        assert_eq!(launched.creations, 1);
        // Depending on when the kill lands, the injection or the wait sees it; either way the
        // launcher answers with an error rather than the exit code.
        let error = launched.result.as_ref().err().expect("the launch fails");
        assert!(error.created);
        assert_eq!(launched.exit_code(), 42);
    }

    #[test]
    fn a_failed_injection_runs_the_program_once_without_mirrord() {
        // An existing file that is not a DLL: creation succeeds and injection fails after it.
        let launched = Launch {
            payload: system_dll("drivers\\etc\\hosts"),
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::Silent)
        }
        .run();
        assert!(
            launched.result.is_ok(),
            "a failed injection is not a failed launch"
        );
        assert_eq!(launched.creations, 1);
        assert_eq!(launched.exit_code(), RAN);
    }

    #[test]
    fn a_failed_injection_ends_the_program_when_the_launcher_says_so() {
        let launched = Launch {
            payload: system_dll("drivers\\etc\\hosts"),
            policy: LayerFailurePolicy::Terminate,
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::Silent)
        }
        .run();
        let error = launched.result.as_ref().err().expect("the launch fails");
        assert!(error.created);
        assert_eq!(launched.creations, 1);
        assert_ne!(launched.exit_code(), RAN, "the program never ran");
    }

    #[test]
    fn a_caller_suspended_child_stays_suspended() {
        // Ready, a failed layer, and a failed injection all hand back the one suspension the
        // creator asked for, and the program runs when the creator resumes it.
        for (layer, payload) in [
            (Layer::Ready, system_dll("version.dll")),
            (Layer::Failed, system_dll("version.dll")),
            (Layer::Silent, system_dll("drivers\\etc\\hosts")),
        ] {
            let launched = Launch {
                caller_suspended: true,
                payload,
                ..Launch::new(InjectionMethod::LoadLibrary, layer)
            }
            .run();
            if let Layer::Ready = layer {
                assert_eq!(launched.progress, ["Ready!"]);
            }
            let process = launched.result.as_ref().expect("launch");
            assert_eq!(resume(process), 1, "exactly the creator's suspension");
            assert_eq!(launched.exit_code(), RAN);
        }
    }

    /// A layer that loaded but never reported, into a process whose primary thread never ran:
    /// the process is not created again, and runs once, or is ended when the launcher says so.
    #[test]
    fn a_silent_layer_before_resume_times_out_without_a_second_creation() {
        let launched = Launch::new(InjectionMethod::LoadLibrary, Layer::Silent).run();
        assert_eq!(launched.creations, 1);
        assert!(launched.progress.is_empty(), "never reported ready");
        assert!(launched.result.is_ok(), "a timeout is not a failed launch");
        assert_eq!(launched.exit_code(), RAN);

        let launched = Launch {
            policy: LayerFailurePolicy::Terminate,
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::Silent)
        }
        .run();
        assert_eq!(launched.creations, 1);
        let error = launched.result.as_ref().err().expect("the launch fails");
        assert!(
            error.created,
            "the process existed, so it must not be created again"
        );
        assert!(
            error.error.to_string().contains("did not report ready"),
            "{error}"
        );
        assert_ne!(launched.exit_code(), RAN, "the program never ran");
    }

    /// A layer that never reported, in a process that is already running: it is left running,
    /// not ended and not created again.
    #[test]
    fn a_silent_layer_after_resume_leaves_the_program_running() {
        let launched = Launch {
            command: OUTLIVES_THE_WAIT,
            ..Launch::new(InjectionMethod::Apc, Layer::Silent)
        }
        .run();
        assert_eq!(launched.creations, 1);
        assert_eq!(
            launched.progress,
            ["layer loading on resume"],
            "done before the program ran, and not again"
        );
        assert!(launched.result.is_ok(), "a timeout is not a failed launch");
        assert_eq!(launched.exit_code(), RAN, "the program ran to its end");
    }

    /// A layer waiting for a debugger reports ready whenever one attaches, so the launcher waits
    /// for it past its usual timeout, but only for the child the setting names.
    #[test]
    fn a_layer_waiting_for_a_debugger_has_no_timeout() {
        let launched = Launch {
            wait_for_debugger: Some("CMD"),
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::LateReady)
        }
        .run();
        assert_eq!(launched.progress, ["Ready!"], "waited for the late layer");
        assert_eq!(launched.exit_code(), RAN);

        let launched = Launch {
            wait_for_debugger: Some("python"),
            ..Launch::new(InjectionMethod::LoadLibrary, Layer::LateReady)
        }
        .run();
        assert!(
            launched.progress.is_empty(),
            "another program's setting keeps the timeout"
        );
        assert_eq!(launched.exit_code(), RAN);
    }

    /// Loading happens on the creator's resume, so nobody waits, and the events must survive in
    /// the child until its layer opens them.
    #[test]
    fn a_caller_suspended_child_keeps_the_events_for_its_layer() {
        let launched = Launch {
            caller_suspended: true,
            ..Launch::new(InjectionMethod::Apc, Layer::Silent)
        }
        .run();
        assert_eq!(launched.progress, ["layer loading queued"]);
        let process = launched.result.as_ref().expect("launch");

        let name =
            CString::new(format!("mirrord_layer_init_{}", process.dwProcessId)).expect("name");
        let event = unsafe { OpenEventA(SYNCHRONIZE, FALSE, name.as_ptr()) };
        assert!(
            !event.is_null(),
            "the child holds the readiness event after the launcher let go"
        );
        unsafe { CloseHandle(event) };

        assert_eq!(resume(process), 1, "exactly the creator's suspension");
        assert_eq!(launched.exit_code(), RAN);
    }

    /// A crash monitor that takes one registration and acknowledges it.
    ///
    /// # Returns
    ///
    /// The monitor's address, and its thread, which hands over the registration it took, or
    /// `None` when none arrived in time.
    fn fake_monitor() -> (String, JoinHandle<Option<Registration>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let address = listener.local_addr().expect("address").to_string();
        let monitor = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if Instant::now() > deadline {
                            return None;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream.set_nonblocking(false).expect("blocking");
            let registration = read_registration(&mut stream).expect("registration");
            stream.write_all(&[ACK_READY]).expect("ack");
            Some(registration)
        });
        (address, monitor)
    }

    /// A layer that is only late is a slow start whatever the launcher does with the process, so
    /// ending it under `exec` files no crash and shows no dialog. The launcher finds the monitor
    /// in the environment it gave the child.
    #[test]
    fn a_readiness_timeout_is_reported_as_a_slow_start_under_either_policy() {
        for (policy, says) in [
            (LayerFailurePolicy::Terminate, "mirrord ended it"),
            (LayerFailurePolicy::RunWithoutMirrord, "keeps running"),
        ] {
            let (address, monitor) = fake_monitor();
            let mut launch = Launch {
                policy,
                ..Launch::new(InjectionMethod::LoadLibrary, Layer::Silent)
            };
            launch
                .environment
                .set(MIRRORD_LAYER_CRASH_MONITOR_ADDR, address);
            let launched = launch.run();
            assert_eq!(launched.creations, 1);

            let registration = monitor
                .join()
                .expect("monitor")
                .expect("the launcher reported to the monitor");
            match registration.init_report {
                Some(InitReport::SlowStart(reason)) => {
                    assert!(reason.contains(says), "{policy:?}: {reason}")
                }
                report => panic!("{policy:?} filed {report:?}"),
            }
        }
    }
}

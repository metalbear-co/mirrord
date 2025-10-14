//! Module responsible for registering hooks targetting process creation.

use std::{ffi::OsString, sync::OnceLock};

use base64::{Engine, engine::general_purpose::URL_SAFE as BASE64_URL_SAFE};
use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};
use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    process::{environment::environment_from_lpvoid, pipe::{forward_pipe_raw_handle_threaded, PipeConfig}},
    proxy_connection::PROXY_CONNECTION,
    setup::layer_setup,
    socket::{SHARED_SOCKETS_ENV_VAR, shared_sockets},
};
use str_win::string_to_u16_buffer;
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, LPVOID},
        ntdef::{HANDLE, LPCWSTR, LPWSTR, NULL},
    },
    um::{
        handleapi::{CloseHandle, SetHandleInformation},
        minwinbase::LPSECURITY_ATTRIBUTES,
        namedpipeapi::CreatePipe,
        processenv::{GetEnvironmentStringsW, GetStdHandle},
        processthreadsapi::{LPPROCESS_INFORMATION, LPSTARTUPINFOW, ResumeThread},
        winbase::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, HANDLE_FLAG_INHERIT,
            STARTF_USESTDHANDLES, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        },
        winnt::PHANDLE,
    },
};

use crate::{
    MIRRORD_LAYER_CHILD_PROCESS_CONFIG_BASE64, MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID,
    MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID, MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR, apply_hook,
};

type CreateProcessInternalWType = unsafe extern "system" fn(
    HANDLE,
    LPCWSTR,
    LPWSTR,
    LPSECURITY_ATTRIBUTES,
    LPSECURITY_ATTRIBUTES,
    BOOL,
    DWORD,
    LPVOID,
    LPCWSTR,
    LPSTARTUPINFOW,
    LPPROCESS_INFORMATION,
    PHANDLE,
) -> BOOL;

static CREATE_PROCESS_INTERNAL_W_ORIGINAL: OnceLock<&CreateProcessInternalWType> = OnceLock::new();

/// NOTE(gabriela): IDA resolves this as a `fastcall` over a `stdcall`
/// but that should be irrelevant on x64?
///
/// # Pseudo-code for call (no type fix-ups)
///
/// ```cpp
/// return CreateProcessInternalW(
///           0i64, // NOTE(gabriela): user token
///           lpApplicationName,
///           lpCommandLine,
///           (__int64)lpProcessAttributes,
///           (__int64)lpThreadAttributes,
///           bInheritHandles,
///           dwCreationFlags,
///           (PWSTR)lpEnvironment,
///           (WCHAR *)lpCurrentDirectory,
///           (__int64)lpStartupInfo,
///           (ULONG_PTR)lpProcessInformation);
/// ```
unsafe extern "system" fn create_process_internal_w_hook(
    user_token: HANDLE,
    application_name: LPCWSTR,
    command_line: LPWSTR,
    process_attributes: LPSECURITY_ATTRIBUTES,
    thread_attributres: LPSECURITY_ATTRIBUTES,
    inherit_handles: BOOL,
    mut creation_flags: DWORD,
    mut environment: LPVOID,
    current_directory: LPCWSTR,
    startup_info: LPSTARTUPINFOW,
    process_information: LPPROCESS_INFORMATION,
    restricted_user_token: PHANDLE,
) -> BOOL {
    unsafe {
        if environment.is_null() {
            // Provide pointer to current environment strings, in wide.
            environment = GetEnvironmentStringsW() as _;

            // Notify later steps that the environment is wide
            creation_flags |= CREATE_UNICODE_ENVIRONMENT;
        }

        let is_w16_env = (creation_flags & CREATE_UNICODE_ENVIRONMENT) != 0;
        let rust_environment = environment_from_lpvoid(is_w16_env, environment);

        let parent_process_id = std::process::id();
        let env_parent_process_id =
            format!("{MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID}={parent_process_id}");

        let proxy_addr = PROXY_CONNECTION
            .get()
            .expect("Couldn't get proxy connection")
            .proxy_addr()
            .to_string();
        let env_proxy_entry = format!("{MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR}={proxy_addr}");

        let layer_id = PROXY_CONNECTION
            .get()
            .expect("Couldn't get proxy connection")
            .layer_id()
            .0;
        let env_layer_id = format!("{MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID}={layer_id}");

        let cfg_base64 = layer_setup()
            .layer_config()
            .encode()
            .expect("Couldn't encode layer config");
        let env_cfg_entry = format!("{MIRRORD_LAYER_CHILD_PROCESS_CONFIG_BASE64}={cfg_base64}");

        // Prepare shared sockets environment variable
        let env_shared_sockets = match shared_sockets() {
            Ok(sockets) if !sockets.is_empty() => {
                let socket_count = sockets.len();
                match bincode::encode_to_vec(sockets, bincode::config::standard())
                    .map(|bytes| BASE64_URL_SAFE.encode(bytes))
                {
                    Ok(encoded) => {
                        tracing::debug!("Sharing {} sockets with child process", socket_count);
                        Some(format!("{SHARED_SOCKETS_ENV_VAR}={encoded}"))
                    }
                    Err(e) => {
                        tracing::warn!("Failed to encode shared sockets: {}", e);
                        None
                    }
                }
            }
            Ok(_) => {
                tracing::trace!("No sockets to share with child process");
                None
            }
            Err(e) => {
                tracing::warn!("Failed to get shared sockets: {}", e);
                None
            }
        };

        // Delete the entries if they already exist first
        let mut rust_environment: Vec<String> = rust_environment
            .into_iter()
            .filter(|x| {
                !(x.starts_with(MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR)
                    || x.starts_with(MIRRORD_LAYER_CHILD_PROCESS_CONFIG_BASE64)
                    || x.starts_with(MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID)
                    || x.starts_with(MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID)
                    || x.starts_with(SHARED_SOCKETS_ENV_VAR))
            })
            .collect();

        // Add the environment entries
        rust_environment.push(env_parent_process_id);
        rust_environment.push(env_layer_id);
        rust_environment.push(env_proxy_entry);
        rust_environment.push(env_cfg_entry);

        // Add shared sockets if available
        if let Some(shared_sockets_env) = env_shared_sockets {
            rust_environment.push(shared_sockets_env);
        }

        // Finally create the final environment
        let rust_environment: Vec<Vec<u16>> = rust_environment
            .into_iter()
            .map(string_to_u16_buffer)
            .collect();

        // Create contiguous vector of environment
        let mut windows_environment: Vec<u16> = Vec::new();
        for entry in rust_environment {
            // `entry` is already a null-terminated vector
            windows_environment.extend(entry);
        }
        // Final null terminator
        windows_environment.push(0);

        // Set environment variable
        environment = windows_environment.as_mut_ptr() as _;

        creation_flags |= CREATE_SUSPENDED;

        // Create pipes for stdout and stderr redirection
        let mut stdout_read: HANDLE = NULL;
        let mut stdout_write: HANDLE = NULL;
        let mut stderr_read: HANDLE = NULL;
        let mut stderr_write: HANDLE = NULL;

        let pipes_created = CreatePipe(&mut stdout_read, &mut stdout_write, std::ptr::null_mut(), 0) != 0
                && CreatePipe(&mut stderr_read, &mut stderr_write, std::ptr::null_mut(), 0) != 0
                // Make the parent handles non-inheritable
                && SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT, 0) != 0
                && SetHandleInformation(stderr_read, HANDLE_FLAG_INHERIT, 0) != 0;

        // Modify startup info to redirect stdout/stderr if pipes were created successfully
        let mut modified_startup_info;
        let startup_info_ptr = if pipes_created {
            // Clone the original startup info
            modified_startup_info = *startup_info;
            modified_startup_info.dwFlags |= STARTF_USESTDHANDLES;
            modified_startup_info.hStdOutput = stdout_write;
            modified_startup_info.hStdError = stderr_write;
            &mut modified_startup_info
        } else {
            tracing::warn!("Failed to create pipes for child process stdout/stderr redirection");
            startup_info
        };

        let original = CREATE_PROCESS_INTERNAL_W_ORIGINAL.get().unwrap();
        let res = original(
            user_token,
            application_name,
            command_line,
            process_attributes,
            thread_attributres,
            inherit_handles,
            creation_flags,
            environment,
            current_directory,
            startup_info_ptr,
            process_information,
            restricted_user_token,
        );

        let dll_path = std::env::var("MIRRORD_LAYER_FILE").unwrap();

        let injector_process =
            InjectorOwnedProcess::from_pid((*process_information).dwProcessId).unwrap();
        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path.clone());
        let _ = syringe.inject(payload_path).unwrap();

        // Close child-side handles and start pipe forwarding if pipes were created
        if pipes_created {
            CloseHandle(stdout_write);
            CloseHandle(stderr_write);

            // Get parent stdout and stderr handles
            let parent_stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            let parent_stderr = GetStdHandle(STD_ERROR_HANDLE);

            // Start pipe forwarding using default config
            let config = PipeConfig::default();
            forward_pipe_raw_handle_threaded(stdout_read as usize, parent_stdout as usize, config.clone());
            forward_pipe_raw_handle_threaded(stderr_read as usize, parent_stderr as usize, config);
        }

        ResumeThread((*process_information).hThread);

        res
    }
}

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    // NOTE(gabriela): handling this at syscall level is super cumbersome
    // and undocumented, so I'd have to reverse engineer CreateProcessInternalW
    // which is like, 3000-ish lines without type fixups, and we shipped this before
    // and in like, 5 years, we had no issues (previous job).
    // so it should work here too.
    apply_hook!(
        guard,
        "kernelbase",
        "CreateProcessInternalW",
        create_process_internal_w_hook,
        CreateProcessInternalWType,
        CREATE_PROCESS_INTERNAL_W_ORIGINAL
    )?;

    Ok(())
}

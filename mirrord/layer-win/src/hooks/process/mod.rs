//! Module responsible for registering hooks targetting process creation.

use std::{ffi::OsString, sync::OnceLock};

use base64::{Engine, engine::general_purpose::URL_SAFE as BASE64_URL_SAFE};
use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};
use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{proxy_connection::PROXY_CONNECTION, setup::layer_setup, socket::{SHARED_SOCKETS_ENV_VAR, SOCKETS, UserSocket}};
use str_win::{string_to_u16_buffer, u8_multi_buffer_to_strings, u16_multi_buffer_to_strings};
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, LPVOID},
        ntdef::{HANDLE, LPCWSTR, LPWSTR},
    },
    um::{
        minwinbase::LPSECURITY_ATTRIBUTES,
        processenv::GetEnvironmentStringsW,
        processthreadsapi::{LPPROCESS_INFORMATION, LPSTARTUPINFOW, ResumeThread},
        winbase::{CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT},
        winnt::PHANDLE,
    },
};

use crate::{
    MIRRORD_LAYER_CHILD_PROCESS_CONFIG_BASE64, MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID,
    MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID, MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR, apply_hook,
};

fn environment_from_lpvoid(is_w16_env: bool, environment: LPVOID) -> Vec<String> {
    unsafe {
        let terminator_size = if is_w16_env { 4 } else { 2 };
        let environment: *mut u8 = environment as *mut u8;
        let mut environment_len = 0;

        // NOTE(gabriela): when going through `CreateProcessA`, there'd be a limit of
        // 32767 bytes. `CreateProcessW` doesn't have this, because, ridiculously, it is quite
        // common for the environment to be *way* larger than that. mirrord is a perfect example
        // which is MUCH more large. So we're gonna loop into infinity willingly. oops?
        for i in 0.. {
            let slice = std::slice::from_raw_parts(environment.add(i), terminator_size);
            if slice.iter().all(|x| *x == 0) {
                environment_len = i + terminator_size;
                break;
            }
        }

        if is_w16_env {
            let environment =
                std::slice::from_raw_parts::<u16>(environment as _, environment_len / 2);
            u16_multi_buffer_to_strings(environment)
        } else {
            let environment = std::slice::from_raw_parts(environment, environment_len);
            u8_multi_buffer_to_strings(environment)
        }
    }
}

/// Converts the SOCKETS map into a vector of pairs (SOCKET, UserSocket) for serialization
fn shared_sockets() -> Result<Vec<(u64, UserSocket)>, Box<dyn std::error::Error>> {
    Ok(SOCKETS
        .lock()
        .map_err(|e| format!("Failed to lock sockets: {}", e))?
        .iter()
        .map(|(socket, user_socket)| (*socket as u64, user_socket.as_ref().clone()))
        .collect())
}

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
            startup_info,
            process_information,
            restricted_user_token,
        );

        let dll_path = std::env::var("MIRRORD_LAYER_FILE").unwrap();

        let injector_process =
            InjectorOwnedProcess::from_pid((*process_information).dwProcessId).unwrap();
        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path.clone());
        let _ = syringe.inject(payload_path).unwrap();

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

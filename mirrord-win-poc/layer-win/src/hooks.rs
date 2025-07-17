use std::{ffi::OsString, os::windows::ffi::OsStrExt};

use asdf_overlay_hook::DetourHook;
use once_cell::sync::OnceCell;
// use tracing::{debug, trace};
use windows::core::PCWSTR;

#[link(name = "Kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    // DWORD GetEnvironmentVariableW(LPCWSTR lpName,LPWSTR  lpBuffer,DWORD   nSize);
    fn GetEnvironmentVariableW(lpName: *const u16, lpBuffer: *mut u16, nSize: i32) -> i32;
}

struct Hook {
    get_environment_variable_w: DetourHook<GetEnvironmentVariableWFn>,
}

static HOOK: OnceCell<Hook> = OnceCell::new();
type GetEnvironmentVariableWFn = unsafe extern "system" fn(*const u16, *mut u16, i32) -> i32;

pub fn install_hooks() -> anyhow::Result<()> {
    HOOK.get_or_try_init(|| unsafe {
        // println!("hooking GetEnvironmentVariableW");
        let get_environment_variable_w = DetourHook::attach(
            GetEnvironmentVariableW as _,
            hooked_get_environment_variable_w as _,
        )?;

        Ok::<_, anyhow::Error>(Hook {
            get_environment_variable_w,
        })
    })?;

    Ok(())
}

// #[tracing::instrument]
#[allow(non_snake_case, unused_variables)]
extern "system" fn hooked_get_environment_variable_w(
    lpName: *const u16,
    lpBuffer: *mut u16,
    nSize: i32,
) -> i32 {
    let name = unsafe { PCWSTR::from_raw(lpName).to_string() }.unwrap();
    // println!("GetEnvironmentVariableW({name:}) called");

    match try_hijack_env_key(name, lpBuffer, nSize) {
        Ok(hijacked_val_len) => hijacked_val_len,
        Err(_) => unsafe {
            HOOK.wait().get_environment_variable_w.original_fn()(lpName, lpBuffer, nSize)
        },
    }
}

fn try_hijack_env_key(key_name: String, buffer_ptr: *mut u16, buffer_size: i32) -> Result<i32, ()> {
    const HIJACKED_KEY: &str = "HIJACK_ME";
    if key_name.ne(HIJACKED_KEY) {
        return Err(());
    }

    const HIJACKED_VAL: &str = "HIJACKED";
    let new_val = OsString::from(HIJACKED_VAL)
        .as_os_str()
        .encode_wide()
        .chain([0u16]) // null-terminator
        .collect::<Vec<u16>>();
    if buffer_size < new_val.len() as i32 {
        println!("Could not hijack value, buffer too small, fallback to original impl.");
        return Err(());
    }

    unsafe {
        std::ptr::copy(new_val.as_ptr(), buffer_ptr, buffer_size as usize);
    }

    Ok(new_val.len() as i32)
}

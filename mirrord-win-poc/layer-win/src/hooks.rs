use std::{ffi::OsString, os::windows::ffi::OsStrExt};

use asdf_overlay_hook::DetourHook;
use once_cell::sync::OnceCell;
// use tracing::{debug, trace};
use windows::core::{PCWSTR, PWSTR};

#[link(name = "Kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    // DWORD GetEnvironmentVariableW(LPCWSTR lpName,LPWSTR  lpBuffer,DWORD   nSize);
    fn GetEnvironmentVariableW(lpName: *const u16, lpBuffer: *mut u16, nSize: i32) -> i32;
    // fn GetEnvironmentVariableW(lpName: *const PCWSTR, lpBuffer: *const PCWSTR, nSize: i32) ->
    // i32;
}

struct Hook {
    get_environment_variable_w: DetourHook<GetEnvironmentVariableWFn>,
}

static HOOK: OnceCell<Hook> = OnceCell::new();
type GetEnvironmentVariableWFn = unsafe extern "system" fn(*const u16, *mut u16, i32) -> i32;

pub fn install_hooks() -> anyhow::Result<()> {
    HOOK.get_or_try_init(|| unsafe {
        println!("hooking GetEnvironmentVariableW");
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
    // trace!("GetEnvironmentVariableW called");
    let name = unsafe { PCWSTR::from_raw(lpName).to_string() }.unwrap();
    println!("GetEnvironmentVariableW({name:}) called");

    if name.eq("TEST") {
        println!("HIJACKING H4x0r");
        // HIJACK TEST value
        let new_val = {
            let mut v = OsString::from("HIJACKED")
                .as_os_str()
                .encode_wide()
                .collect::<Vec<u16>>();
            v.push(0u16); // null-terminator
            v
        };
        if nSize > new_val.len() as i32 {
            unsafe {
                std::ptr::copy(new_val.as_ptr(), lpBuffer, nSize as usize);
            }
        } else {
            println!("Could not hijack value, unsupported buffer too small");
        }

        let buffer_val = unsafe { PWSTR::from_raw(lpBuffer).to_string() }.unwrap();
        println!("value of buffer {buffer_val:}");

        return buffer_val.len() as i32;
    }

    let orig =
        unsafe { HOOK.wait().get_environment_variable_w.original_fn()(lpName, lpBuffer, nSize) };

    println!("GetEnvironmentVariableW orig returned: {orig}");

    orig
}

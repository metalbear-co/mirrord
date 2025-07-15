use asdf_overlay_hook::DetourHook;
use once_cell::sync::OnceCell;
// use tracing::{debug, trace};
use windows::core::PCWSTR;

#[link(name = "Kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn GetEnvironmentVariableW(lpName: *const u16, lpBuffer: *mut u16, nSize: i32) -> i32;
    // fn GetEnvironmentVariableW(lpName: *const PCWSTR, lpBuffer: *const PCWSTR, nSize: i32) ->
    // i32;
}

struct Hook {
    get_environment_variable_w: DetourHook<GetEnvironmentVariableWFn>,
}

static HOOK: OnceCell<Hook> = OnceCell::new();
type GetEnvironmentVariableWFn = unsafe extern "system" fn(*const u16, *mut u16, i32) -> i32;

pub fn hook() -> anyhow::Result<()> {
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
    println!("GetEnvironmentVariableW called");

    let name = unsafe { PCWSTR::from_raw(lpName).to_string() }.unwrap();
    println!("key={:#?}", name);

    let orig =
        unsafe { HOOK.wait().get_environment_variable_w.original_fn()(lpName, lpBuffer, nSize) };

    println!("GetEnvironmentVariableW orig returned: {orig}");

    orig
}

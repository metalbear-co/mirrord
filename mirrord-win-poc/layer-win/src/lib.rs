use asdf_overlay_hook::DetourHook;
use once_cell::sync::OnceCell;
use windows::core::{PCWSTR, PWSTR};

#[link(name = "Kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn GetEnvironmentVariableW(lpName: *const PCWSTR, lpBuffer: *mut PWSTR, nSize: i32) -> i32;
    // fn GetEnvironmentVariableW(lpName: *const PCWSTR, lpBuffer: *const PCWSTR, nSize: i32) ->
    // i32;
}

struct Hook {
    get_environment_variable_w: DetourHook<GetEnvironmentVariableWFn>,
}

static HOOK: OnceCell<Hook> = OnceCell::new();
type GetEnvironmentVariableWFn = unsafe extern "system" fn(*const PCWSTR, *mut PWSTR, i32) -> i32;

pub fn install() {
    // install hooks - read/write envvar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(1, 1);
    }
}

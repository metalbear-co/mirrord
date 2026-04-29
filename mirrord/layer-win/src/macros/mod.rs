/// Wrap windows shared library entry point to only take a closure.
/// Function will be exported as `DllMain` in the process export table.
///
/// # Arguments
///
/// * `module` - [`winapi::shared::minwindef::HMODULE`] given to `DllMain`. Points towards the base
///   address of DLL in virtual memory.
/// * `reason_for_call` - The reason for call ([`winapi::um::winnt::DLL_PROCESS_ATTACH`],
///   [`winapi::um::winnt::DLL_PROCESS_DETACH`], ...) given to `DllMain`.
/// * `reserved` - Depends on `reason_for_call`, [please refer to MSDN](https://learn.microsoft.com/en-us/windows/win32/dlls/dllmain).
#[macro_export]
macro_rules! entry_point {
    ($f: expr) => {
        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        unsafe extern "system" fn DllMain(
            module: winapi::shared::minwindef::HMODULE,
            reason_for_call: winapi::shared::minwindef::DWORD,
            reserved: winapi::shared::minwindef::LPVOID,
        ) -> winapi::shared::minwindef::BOOL {
            $f(module, reason_for_call, reserved).into()
        }
    };
}

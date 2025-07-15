/// Wrap windows shared library entry point to only take a closure.
/// Function will be exported as `DllMain` in the process export table.
///
/// # Arguments
///
/// * `module` - [`winapi::shared::minwindef::HMODULE`] given to `DllMain`. Points towards the base address of DLL in virtual memory.
/// * `reason_for_call` - The reason for call ([`winapi::um::winnt::DLL_PROCESS_ATTACH`], [`winapi::um::winnt::DLL_PROCESS_DETACH`], ...) given to `DllMain`.
#[macro_export]
macro_rules! entry_point {
    ($f: expr) => {
        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        unsafe extern "stdcall" fn DllMain(
            module: winapi::shared::minwindef::HMODULE,
            reason_for_call: winapi::shared::minwindef::DWORD,
            _: winapi::shared::minwindef::LPVOID,
        ) -> winapi::shared::minwindef::BOOL {
            // Coerce values
            $f(module, reason_for_call as u32).into()
        }
    };
}

#[macro_export]
macro_rules! replace {
    ($hook_manager:expr, $func:expr, $detour_function:expr, $detour_type:ty, $hook_fn:expr) => {{
        let intercept = |hook_manager: &mut $crate::hooks::HookManager,
                         symbol_name,
                         detour: $detour_type|
         -> $crate::error::Result<$detour_type> {
            let replaced =
                hook_manager.hook_export_or_any(symbol_name, detour as *mut libc::c_void)?;
            let original_fn: $detour_type = std::mem::transmute(replaced);

            Ok(original_fn)
        };

        let _ = intercept($hook_manager, $func, $detour_function)
            .and_then(|hooked| Ok($hook_fn.set(hooked).unwrap()));
    }};
}

#[macro_export]
macro_rules! replace_symbol {
    ($hook_manager:expr, $func:expr, $detour_function:expr, $detour_type:ty, $hook_fn:expr) => {{
        let intercept = |hook_manager: &mut $crate::hooks::HookManager,
                         symbol_name,
                         detour: $detour_type|
         -> $crate::error::Result<$detour_type> {
            let replaced =
                hook_manager.hook_symbol_main_module(symbol_name, detour as *mut libc::c_void)?;
            let original_fn: $detour_type = std::mem::transmute(replaced);

            Ok(original_fn)
        };

        let _ = intercept($hook_manager, $func, $detour_function)
            .and_then(|hooked| Ok($hook_fn.set(hooked).unwrap()));
    }};
}

#[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
macro_rules! hook_symbol {
    ($hook_manager:expr, $func:expr, $detour_name:expr) => {
        match hook_manager.hook_symbol_main_module($func, $detour_name) {
            Ok(_) => {
                trace!("hooked {func:?} in main module");
            }
            Err(err) => {
                trace!("hook {func:?} in main module failed with err {err:?}");
            }
        }
    };
}

#[macro_export]
macro_rules! graceful_exit {
    ($($arg:tt)+) => {{
        eprintln!($($arg)+);
        graceful_exit!()
    }};
    () => {{
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(std::process::id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .expect("unable to graceful exit");
        panic!()
    }};
}

#[cfg(all(target_os = "linux", not(target_arch = "aarch64")))]
pub(crate) use hook_symbol;

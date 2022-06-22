macro_rules! hook {
    ($interceptor:expr, $func:expr, $detour_name:expr) => {
        $interceptor
            .replace(
                frida_gum::Module::find_export_by_name(None, $func).unwrap(),
                frida_gum::NativePointer($detour_name as *mut libc::c_void),
                frida_gum::NativePointer(std::ptr::null_mut::<libc::c_void>()),
            )
            .unwrap();
    };
}

macro_rules! try_hook {
    ($interceptor:expr, $func:expr, $detour_name:expr) => {
        if let Some(addr) = frida_gum::Module::find_export_by_name(None, $func) {
            match $interceptor.replace(
                addr,
                frida_gum::NativePointer($detour_name as *mut libc::c_void),
                frida_gum::NativePointer(std::ptr::null_mut::<libc::c_void>()),
            ) {
                Err(frida_gum::Error::InterceptorAlreadyReplaced) => {
                    error!("{} already replaced", $func);
                }
                Err(e) => {
                    error!("{} error: {:?}", $func, e);
                }
                Ok(_) => {
                    tracing::trace!("{} hooked", $func);
                }
            }
        }
    };
}

pub(crate) use hook;
pub(crate) use try_hook;

#[macro_export]
macro_rules! hook2 {
    ($interceptor:expr, $detour_name:expr, $detour_function:expr, $detour_type:ty) => {{
        let intercept = |interceptor: &mut frida_gum::interceptor::Interceptor,
                         symbol_name,
                         detour: $detour_type|
         -> Result<$detour_type, LayerError> {
            let function = frida_gum::Module::find_export_by_name(None, symbol_name)
                .ok_or(LayerError::NoExportName(symbol_name.to_string()))?;

            let replaced = interceptor.replace(
                function,
                frida_gum::NativePointer(detour as *mut libc::c_void),
                frida_gum::NativePointer(std::ptr::null_mut()),
            )?;

            let original_fn: $detour_type = std::mem::transmute(replaced);

            Ok(original_fn)
        };

        intercept($interceptor, $detour_name, $detour_function)
    }};
}

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
                    debug!("{} hooked", $func);
                }
            }
        }
    };
}

pub(crate) use hook;
pub(crate) use try_hook;

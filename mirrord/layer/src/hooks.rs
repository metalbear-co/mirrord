use std::{ptr::null_mut, sync::LazyLock};

use frida_gum::{Gum, Module, NativePointer, Process, interceptor::Interceptor};
use tracing::trace;

use crate::{LayerError, Result};

static GUM: LazyLock<Gum> = LazyLock::new(Gum::obtain);

/// Struct for managing the hooks using Frida.
pub(crate) struct HookManager<'a> {
    interceptor: Interceptor,
    modules: Vec<Module>,
    // process is need for Linux build and having different struct between OS feels over kill
    #[allow(dead_code)]
    process: Process<'a>,
}

impl<'a> HookManager<'a> {
    /// Hook the first function exported from a lib that is in modules and is hooked succesfully
    pub(crate) fn hook_any_lib_export(
        &mut self,
        symbol: &str,
        detour: *mut libc::c_void,
        filter: Option<&str>,
    ) -> Result<NativePointer> {
        for module in &self.modules {
            // In this case we only want libs, no "main binaries"
            let module_name = module.name();
            if !module_name.starts_with(filter.unwrap_or("lib")) {
                continue;
            }

            if let Some(function) = module.find_export_by_name(symbol) {
                trace!("found {symbol:?} in {module_name:?}, hooking");
                match self.interceptor.replace(
                    function,
                    NativePointer(detour),
                    NativePointer(null_mut()),
                ) {
                    Ok(original) => return Ok(original),
                    Err(err) => {
                        trace!("hook {symbol:?} in {module_name:?} failed with err {err:?}")
                    }
                }
            }
        }
        Err(LayerError::NoExportName(symbol.to_string()))
    }

    /// Hook an exported symbol, suitable for most libc use cases.
    /// If it fails to hook the first one found, it will try to hook each matching export
    /// until it succeeds.
    pub(crate) fn hook_export_or_any(
        &mut self,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        // First try to hook the default exported one, if it fails, fallback to first lib that
        // provides it.
        let function = Module::find_global_export_by_name(symbol);
        match function {
            Some(func) => self
                .interceptor
                .replace(func, NativePointer(detour), NativePointer(null_mut()))
                .or_else(|_| self.hook_any_lib_export(symbol, detour, None)),
            None => self.hook_any_lib_export(symbol, detour, None),
        }
    }

    #[cfg(target_os = "linux")]
    /// Hook a symbol in the first module (main module, binary)
    pub(crate) fn hook_symbol_main_module(
        &mut self,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        let function = self
            .process
            .main_module
            .find_symbol_by_name(symbol)
            .ok_or_else(|| LayerError::NoSymbolName(symbol.to_string()))?;

        // on Go we use `replace_fast` since we don't use the original function.
        self.interceptor
            .replace_fast(function, NativePointer(detour))
            .map_err(Into::into)
    }

    /// Resolve symbol in main module
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(crate) fn resolve_symbol_main_module(&self, symbol: &str) -> Option<NativePointer> {
        // This can't fail
        self.process.main_module.find_symbol_by_name(symbol)
    }
}

impl<'a> Default for HookManager<'a> {
    fn default() -> Self {
        let mut interceptor = Interceptor::obtain(&GUM);
        interceptor.begin_transaction();
        let process = Process::obtain(&GUM);
        let modules = process.enumerate_modules();
        Self {
            interceptor,
            modules,
            process,
        }
    }
}

impl<'a> Drop for HookManager<'a> {
    fn drop(&mut self) {
        self.interceptor.end_transaction()
    }
}

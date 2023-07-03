use std::{ptr::null_mut, sync::LazyLock};

use frida_gum::{interceptor::Interceptor, Gum, Module, NativePointer};
use tracing::trace;

use crate::{LayerError, Result};

static GUM: LazyLock<Gum> = LazyLock::new(|| unsafe { Gum::obtain() });

/// Struct for managing the hooks using Frida.
pub(crate) struct HookManager<'a> {
    interceptor: Interceptor<'a>,
    modules: Vec<String>,
}

/// Gets available modules in current process.
/// NOTE: Gum must be initialized or it will crash.
fn get_modules() -> Vec<String> {
    Module::enumerate_modules()
        .iter()
        .map(|m| m.name.clone())
        .collect()
}

/// Wrapper function that maps it to our error for convinence.
fn get_export_by_name(module: Option<&str>, symbol: &str) -> Result<NativePointer> {
    Module::find_export_by_name(module, symbol)
        .ok_or_else(|| LayerError::NoExportName(symbol.to_string()))
}

impl<'a> HookManager<'a> {
    /// Hook the first function exported from a lib that is in modules and is hooked succesfully
    fn hook_any_lib_export(
        &mut self,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        for module in &self.modules {
            // In this case we only want libs, no "main binaries"
            if !module.starts_with("lib") {
                continue;
            }
            if let Ok(function) = get_export_by_name(Some(module), symbol) {
                trace!("found {symbol:?} in {module:?}, hooking");
                match self.interceptor.replace(
                    function,
                    NativePointer(detour),
                    NativePointer(null_mut()),
                ) {
                    Ok(original) => return Ok(original),
                    Err(err) => {
                        trace!("hook {symbol:?} in {module:?} failed with err {err:?}")
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
        let function = get_export_by_name(None, symbol)?;

        self.interceptor
            .replace(function, NativePointer(detour), NativePointer(null_mut()))
            .or_else(|_| self.hook_any_lib_export(symbol, detour))
    }

    #[cfg(target_os = "linux")]
    /// Hook a symbol that isn't exported.
    /// It is valuable when hooking internal stuff. (Go)
    fn hook_symbol(
        &mut self,
        module: &str,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        let function = Module::find_symbol_by_name(module, symbol)
            .ok_or_else(|| LayerError::NoSymbolName(symbol.to_string()))?;

        // on Go we use `replace_fast` since we don't use the original function.
        self.interceptor
            .replace_fast(function, NativePointer(detour))
            .map_err(Into::into)
    }

    #[cfg(target_os = "linux")]
    /// Hook a symbol in the first module (main module, binary)
    pub(crate) fn hook_symbol_main_module(
        &mut self,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        // This can't fail
        let module = self.modules.first().unwrap().clone();
        self.hook_symbol(&module, symbol, detour)
    }

    /// Resolve symbol in main module
    #[cfg(target_os = "linux")]
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn resolve_symbol_main_module(&self, symbol: &str) -> Option<NativePointer> {
        // This can't fail
        let module = self.modules.first().unwrap().clone();
        Module::find_symbol_by_name(&module, symbol)
    }
}

impl<'a> Default for HookManager<'a> {
    fn default() -> Self {
        let mut interceptor = Interceptor::obtain(&GUM);
        interceptor.begin_transaction();
        let modules = get_modules();
        Self {
            interceptor,
            modules,
        }
    }
}

impl<'a> Drop for HookManager<'a> {
    fn drop(&mut self) {
        self.interceptor.end_transaction()
    }
}

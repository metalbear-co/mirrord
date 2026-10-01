use std::{ptr::null_mut, sync::LazyLock};

use frida_gum::{
    Gum, Module, NativePointer, Process,
    interceptor::{Interceptor, InvocationContext, ProbeListener},
};
use mirrord_layer_lib::error::{LayerError, Result};
use tracing::trace;

static GUM: LazyLock<Gum> = LazyLock::new(Gum::obtain);

/// A call intercepted by [`HookManager::probe_export_or_any`], before the original function runs.
pub struct ProbedCall<'a>(InvocationContext<'a>);

impl ProbedCall<'_> {
    /// The zero-based `n`th argument, as passed in its register or stack slot.
    pub fn arg(&self, n: u32) -> usize {
        self.0.arg(n)
    }

    /// Replaces the `n`th argument the original function is called with.
    pub fn set_arg(&self, n: u32, value: usize) {
        self.0.set_arg(n, value)
    }
}

/// Listener [`HookManager::probe_export_or_any`] attaches.
struct Probe(fn(&ProbedCall<'_>));

impl ProbeListener for Probe {
    fn on_hit(&mut self, context: InvocationContext) {
        (self.0)(&ProbedCall(context))
    }
}

/// Struct for managing the hooks using Frida.
pub struct HookManager<'a> {
    interceptor: Interceptor,
    modules: Vec<Module>,
    // process is need for Linux build and having different struct between OS feels over kill
    #[allow(dead_code)]
    process: Process<'a>,
}

impl<'a> HookManager<'a> {
    /// Hook the first function exported from a lib that is in modules and is hooked succesfully
    pub fn hook_any_lib_export(
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
        Err(LayerError::NoExportName(symbol.to_owned()))
    }

    /// Runs `on_hit` at the entry of the exported `symbol`, then lets the original function run,
    /// for functions that may never return to the caller.
    ///
    /// A hook installed with [`Self::hook_export_or_any`] replaces the function, and frida records,
    /// per thread, that the replacement is running until it returns. One that never returns
    /// leaves the record behind. That is harmless when the whole process image goes away, but not
    /// when it runs in a `vfork` child sharing the caller's memory, as glibc's `posix_spawn` does
    /// for `execve`: the caller's thread keeps the record, and frida then sends every later call on
    /// that thread straight to the original, skipping the replacement.
    ///
    /// A probe keeps no such record. `on_hit` runs and returns before the original starts, and can
    /// only change the arguments the original is called with, see [`ProbedCall`].
    pub fn probe_export_or_any(&mut self, symbol: &str, on_hit: fn(&ProbedCall<'_>)) -> Result<()> {
        // frida keeps a pointer to the listener for as long as the probe is attached, which is
        // for the rest of the process.
        let probe = Box::leak(Box::new(Probe(on_hit)));

        if let Some(function) = Module::find_global_export_by_name(symbol)
            && let Ok(listener) = self.interceptor.attach_instruction(function, probe)
        {
            // Frida does not keep its own reference to the listener while the probe is attached.
            std::mem::forget(listener);
            trace!("probed {symbol:?}");
            return Ok(());
        }

        for module in &self.modules {
            let module_name = module.name();
            if !module_name.starts_with("lib") {
                continue;
            }

            if let Some(function) = module.find_export_by_name(symbol) {
                trace!("found {symbol:?} in {module_name:?}, probing");
                match self.interceptor.attach_instruction(function, probe) {
                    Ok(listener) => {
                        // Frida does not keep its own reference to the listener while the probe is
                        // attached.
                        std::mem::forget(listener);
                        trace!("probed {symbol:?}");
                        return Ok(());
                    }
                    Err(err) => {
                        trace!("probe {symbol:?} in {module_name:?} failed with err {err:?}")
                    }
                }
            }
        }
        Err(LayerError::NoExportName(symbol.to_owned()))
    }

    /// Hook an exported symbol, suitable for most libc use cases.
    /// If it fails to hook the first one found, it will try to hook each matching export
    /// until it succeeds.
    pub fn hook_export_or_any(
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
    pub fn hook_symbol_main_module(
        &mut self,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        let function = self
            .process
            .main_module()
            .find_symbol_by_name(symbol)
            .ok_or_else(|| LayerError::NoSymbolName(symbol.to_owned()))?;

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
    pub fn resolve_symbol_main_module(&self, symbol: &str) -> Option<NativePointer> {
        // This can't fail
        self.process.main_module().find_symbol_by_name(symbol)
    }

    /// Resolve symbol in the given module
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn resolve_symbol_in_module(
        &self,
        module_name: &str,
        symbol: &str,
    ) -> Option<NativePointer> {
        let Some(module) = self.modules.iter().find(|m| m.name() == module_name) else {
            trace!(module_name, "Module not found");
            return None;
        };
        module.find_symbol_by_name(symbol)
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn hook_symbol_in_module(
        &mut self,
        module: &str,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        let Some(module) = self.modules.iter().find(|m| m.name() == module) else {
            return Err(LayerError::NoModuleName(module.to_owned()));
        };

        let function = module
            .find_symbol_by_name(symbol)
            .ok_or_else(|| LayerError::NoSymbolName(symbol.to_owned()))?;

        // on Go we use `replace_fast` since we don't use the original function.
        self.interceptor
            .replace_fast(function, NativePointer(detour))
            .map_err(Into::into)
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

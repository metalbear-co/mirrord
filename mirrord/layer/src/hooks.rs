use std::{ptr::null_mut, sync::LazyLock};

use frida_gum::{Gum, Module, NativePointer, Process, interceptor::Interceptor};
use tracing::trace;

use crate::{LayerError, Result};

static GUM: LazyLock<Gum> = LazyLock::new(Gum::obtain);


use libc::{mprotect, PROT_READ, PROT_WRITE, PROT_EXEC, c_void};
use std::io;

/// Change memory protection for a page-aligned address range
/// 
/// # Safety
/// - `addr` must be page-aligned
/// - `len` specifies the number of bytes to protect
/// - The memory region must be valid and owned by the process
pub unsafe fn change_mprotect(
    address: *mut c_void,
    len: usize,
    readable: bool,
    writable: bool,
    executable: bool,
) -> io::Result<()> {
    let mut prot = 0;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
    
    // Align address down to page boundary
    let addr_usize = address as usize;
    let aligned_address = addr_usize & !(page_size - 1);
    
    // Calculate aligned size
    let end_address = addr_usize + len - 1;
    let aligned_size = (1 + ((end_address - aligned_address) / page_size)) * page_size;

    if readable {
        prot |= PROT_READ;
    }
    if writable {
        prot |= PROT_WRITE;
    }
    if executable {
        prot |= PROT_EXEC;
    }
    
    let result = mprotect(aligned_address as *mut libc::c_void, aligned_size, prot);
    
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// Helper to get page size
pub fn get_page_size() -> usize {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

// Helper to align address to page boundary
pub fn align_to_page(addr: usize) -> usize {
    let page_size = get_page_size();
    addr & !(page_size - 1)
}

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

    /// Resolve symbol in the given module
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(crate) fn resolve_symbol_in_module(
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
    pub(crate) fn hook_symbol_in_module(
        &mut self,
        module: &str,
        symbol: &str,
        detour: *mut libc::c_void,
    ) -> Result<NativePointer> {
        let Some(module) = self.modules.iter().find(|m| m.name() == module) else {
            return Err(LayerError::NoModuleName(module.to_string()));
        };

        let mut function = module
            .find_symbol_by_name(symbol)
            .ok_or_else(|| LayerError::NoSymbolName(symbol.to_string()))?;

        // on Go we use `replace_fast` since we don't use the original function.
        tracing::warn!("hooking with not fast replace");
        // self.interceptor
        //     .replace_fast(function, NativePointer(detour))?;

        unsafe { change_mprotect(function.0, 30, true, true, true).unwrap(); }
        use frida_gum::instruction_writer::{TargetInstructionWriter, InstructionWriter};
        let writer = frida_gum::instruction_writer::TargetInstructionWriter::new(function.0 as u64);
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.put_nop();
        writer.flush();
        function = NativePointer(writer.code_offset() as *mut libc::c_void);
        drop(writer);
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

//! Minidump writers.
//!
//! Two paths exist. The out-of-process path is the safe one. The monitor calls it with a handle to
//! the crashed process. The in-process path is the last-resort fallback. The crash handler calls it
//! on the faulting process itself.
//!
//! Both wrap `MiniDumpWriteDump` from `dbghelp`. The default content is rich but bounded. The
//! `fullmemory` toggle upgrades it to a full-memory dump.

use std::os::windows::io::RawHandle;

use windows_sys::Win32::System::Diagnostics::Debug::{
    MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE, MiniDumpWithFullMemory, MiniDumpWithHandleData,
    MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithProcessThreadData, MiniDumpWithThreadInfo,
    MiniDumpWithUnloadedModules, MiniDumpWriteDump,
};

/// The default minidump content. Rich enough to debug, bounded in size.
const DEFAULT_DUMP_TYPE: MINIDUMP_TYPE = MiniDumpWithThreadInfo
    | MiniDumpWithHandleData
    | MiniDumpWithUnloadedModules
    | MiniDumpWithIndirectlyReferencedMemory
    | MiniDumpWithProcessThreadData;

/// Writes a minidump of a process to an open file.
///
/// The out-of-process monitor passes a handle to the crashed process. The in-process fallback
/// passes the current process.
///
/// # Arguments
///
/// * `process` - a handle to the process to dump.
/// * `process_id` - that process's id.
/// * `file` - an open, writable file handle for the `.dmp`.
/// * `exception` - optional exception information for the faulting thread.
/// * `full_memory` - whether to upgrade to a full-memory dump.
///
/// # Returns
///
/// `true` when the dump was written.
pub fn write_dump(
    process: RawHandle,
    process_id: u32,
    file: RawHandle,
    exception: Option<*mut MINIDUMP_EXCEPTION_INFORMATION>,
    full_memory: bool,
) -> bool {
    let mut dump_type = DEFAULT_DUMP_TYPE;
    if full_memory {
        dump_type |= MiniDumpWithFullMemory;
    }

    let exception = exception.unwrap_or(std::ptr::null_mut());
    let ok = unsafe {
        MiniDumpWriteDump(
            process as _,
            process_id,
            file as _,
            dump_type,
            exception,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    ok != 0
}

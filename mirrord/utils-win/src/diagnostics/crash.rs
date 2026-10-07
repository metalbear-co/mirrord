//! In-process crash handler.
//!
//! This installs a narrow exception handler into the layer'd process. On a severe fault it writes a
//! text crash record and a minidump. Then it chains to whatever handler was there before. So the
//! host process and Windows Error Reporting are unaffected.
//!
//! ## Why it is written the way it is
//!
//! The handler runs in a broken process. So it is allocation-free. It formats into a fixed stack
//! buffer. It writes with raw `WriteFile`. It resolves the faulting module from a cached
//! [`ModuleTable`] so it never calls the loader. A re-entrancy guard turns a fault inside the
//! handler into a clean bail-out instead of an infinite loop.
//!
//! ## Two registrations, on purpose
//!
//! The unhandled-exception filter is the primary record and dump producer. It fires only on a
//! genuinely unhandled exception. So it never false-positives on a first-chance fault that some
//! `__try`/`__except` downstream was going to handle.
//!
//! The vectored handler is a narrow net. It only acts on a stack overflow, where first-chance is
//! already fatal and recovery is impossible. The other severe codes are left to the filter; it also
//! logs them as first-chance faults for diagnosis, staying quiet when a debugger is attached.
//!
//! Fail-fast (`0xC0000409`) and heap corruption (`0xC0000374`) bypass both. The out-of-process
//! monitor's death-detector is what catches those.
//!
//! ## Two installation steps, on purpose
//!
//! [`install_filter`] puts the unhandled-exception filter into the OS slot. The layer calls it
//! inside `DllMain`, before it enables its hooks: once the `SetUnhandledExceptionFilter` hook is
//! live, a call through the public API only changes the chain target (see
//! [`adopt_previous_filter`]), so the filter has to reach the OS before that. It loads nothing and
//! opens nothing, which is what makes it safe under the loader lock.
//!
//! [`register_monitor`] does the part a short-lived process needs covered: artifact paths, the
//! monitor registration and the vectored handler. It opens a socket, so it runs on the layer's
//! startup worker, before the layer reports ready. Until it has run, the filter only chains.
//!
//! [`capture_modules`] does the rest once the layer has reported ready: `dbghelp` and the module
//! table that resolves faulting addresses. Neither is needed to start the target, so neither holds
//! it up. A crash before it has run gets a record without module names, and the monitor's own
//! module inventory, taken from outside, still names them.
//!
//! ## Out-of-process dumping
//!
//! The safe dump is the monitor's job: dumping a crashed process from the outside avoids running
//! dump code in a broken address space. When [`register_monitor`] registered with a monitor, a
//! crash signals it and waits for the out-of-process dump. The in-process dump here is the
//! fallback, used only when there is no monitor or it does not acknowledge in time.

use std::{
    fmt::Write as _,
    net::SocketAddr,
    os::windows::io::RawHandle,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use str_win::string_to_u16_buffer;
use winapi::{
    ctypes::c_void,
    shared::{
        minwindef::{DWORD, FALSE},
        ntdef::LONG,
    },
    um::{
        debugapi::IsDebuggerPresent,
        errhandlingapi::{
            AddVectoredExceptionHandler, LPTOP_LEVEL_EXCEPTION_FILTER,
            RemoveVectoredExceptionHandler, SetUnhandledExceptionFilter,
        },
        fileapi::{CREATE_ALWAYS, CreateFileW, WriteFile},
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        minwinbase::{EXCEPTION_ACCESS_VIOLATION, EXCEPTION_STACK_OVERFLOW},
        processenv::GetStdHandle,
        processthreadsapi::{
            GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, SetThreadStackGuarantee,
        },
        winbase::STD_ERROR_HANDLE,
        winnt::{
            EXCEPTION_POINTERS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GENERIC_WRITE, HANDLE,
            RtlCaptureStackBackTrace,
        },
    },
    vc::excpt::EXCEPTION_CONTINUE_SEARCH,
};
use windows_sys::Win32::System::Diagnostics::Debug::MINIDUMP_EXCEPTION_INFORMATION;

use self::codes::{code_name, exception_code, is_severe};
use super::{
    monitor::{MonitorChannel, Registration},
    report::is_native_fault,
};
use crate::{fixed_buf::FixedBuf, modules::ModuleTable};

mod codes;

/// Stack reserved for the handler so a stack overflow still has room to run it.
const HANDLER_STACK_GUARANTEE: DWORD = 64 * 1024;
/// Frame count captured for the short stack.
const STACK_FRAMES: usize = 32;

/// The installed crash state. Read-only once set, from the handler.
///
/// Function pointers are already `Sync`, so the previous filter is stored directly. The paths are
/// opened lazily in the handler, so no crash-time file is created unless a crash happens.
struct CrashState {
    /// Null-terminated wide path for the crash record. Opened lazily on a crash.
    record_path: Vec<u16>,
    /// Null-terminated wide path for the in-process dump. Opened lazily on a crash.
    dump_path: Vec<u16>,
    /// The out-of-process dump channel, when a monitor is configured.
    monitor: Option<MonitorChannel>,
    /// Whether to upgrade the dump to full memory.
    full_memory: bool,
}

static STATE: OnceLock<CrashState> = OnceLock::new();
/// The module table for loader-free address resolution, once [`capture_modules`] has run.
static MODULES: OnceLock<ModuleTable> = OnceLock::new();
static VEH_HANDLE: AtomicUsize = AtomicUsize::new(0);
/// The filter our handler chains to, as a `usize`.
///
/// It starts as whatever filter was installed before us. The layer's `SetUnhandledExceptionFilter`
/// hook updates it when the target's runtime tries to install its own filter, so we still chain to
/// the latest while keeping ours as the OS top-level filter.
static PREVIOUS_FILTER: AtomicUsize = AtomicUsize::new(0);
/// Set while [`unhandled_filter`] holds the OS slot.
static FILTER_INSTALLED: AtomicBool = AtomicBool::new(false);
/// Set once the record has been produced. Keeps one record per process.
static HANDLED: AtomicBool = AtomicBool::new(false);
/// Set while inside the handler. Turns a handler fault into a bail-out.
static IN_HANDLER: AtomicBool = AtomicBool::new(false);
/// Set when layer init failed. Suppresses the clean-shutdown signal, so the process death is never
/// mistaken for a normal close even if the init-failure signal raced the exit.
static INIT_FAILED: AtomicBool = AtomicBool::new(false);

/// Converts an optional filter to its raw address.
fn filter_to_usize(filter: LPTOP_LEVEL_EXCEPTION_FILTER) -> usize {
    filter.map_or(0, |filter| filter as usize)
}

/// Converts a raw address back to an optional filter. `0` is `None`.
fn filter_from_usize(value: usize) -> LPTOP_LEVEL_EXCEPTION_FILTER {
    // SAFETY: the value is either 0 (None via the null niche) or an address previously obtained
    // from a valid `LPTOP_LEVEL_EXCEPTION_FILTER`.
    unsafe { std::mem::transmute::<usize, LPTOP_LEVEL_EXCEPTION_FILTER>(value) }
}

/// Records a newly-requested unhandled-exception filter as our chain target.
///
/// The layer's `SetUnhandledExceptionFilter` hook calls this instead of letting the caller replace
/// the OS top-level filter. Ours stays installed; the caller's filter becomes what we chain to. The
/// previously-stored filter is returned, mimicking the real API's contract.
///
/// The hook is enabled only after [`install_filter`] has put ours into the OS slot, so every filter
/// the target registers, before or after that, ends up in the chain.
///
/// # Arguments
///
/// * `new` - the filter the caller tried to install.
///
/// # Returns
///
/// The filter that was the chain target before this call.
pub fn adopt_previous_filter(new: LPTOP_LEVEL_EXCEPTION_FILTER) -> LPTOP_LEVEL_EXCEPTION_FILTER {
    let old = PREVIOUS_FILTER.swap(filter_to_usize(new), Ordering::SeqCst);
    filter_from_usize(old)
}

/// Puts `unhandled_filter` into the OS unhandled-exception slot.
///
/// The layer calls this inside `DllMain`, before it enables the `SetUnhandledExceptionFilter`
/// hook, so the call below still reaches the OS. Whatever filter the target registered earlier (a
/// static initializer, a TLS callback) becomes the chain target. The call loads no module and
/// opens nothing, so it is safe under the loader lock.
///
/// Idempotent. Until [`register_monitor`] has run, the filter produces nothing and only chains.
pub fn install_filter() {
    if FILTER_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    let previous = unsafe { SetUnhandledExceptionFilter(Some(unhandled_filter)) };
    PREVIOUS_FILTER.store(filter_to_usize(previous), Ordering::SeqCst);
}

/// Gives the OS slot back to the filter [`install_filter`] displaced.
///
/// For a layer whose startup failed after [`install_filter`] and before its hooks were enabled, so
/// the call below still reaches the OS. The layer stays mapped but inert, and the target's own
/// filter owns the slot again.
pub fn restore_filter() {
    if !FILTER_INSTALLED.swap(false, Ordering::SeqCst) {
        return;
    }

    let previous = filter_from_usize(PREVIOUS_FILTER.swap(0, Ordering::SeqCst));
    unsafe { SetUnhandledExceptionFilter(previous) };
}

/// Options for [`register_monitor`].
pub struct InstallOptions {
    /// Directory for crash artifacts. Usually the layer log directory.
    pub directory: PathBuf,
    /// Process name used in artifact file names.
    pub process_name: String,
    /// Whether to upgrade the minidump to a full-memory dump.
    pub full_memory: bool,
    /// The crash monitor endpoint and this process's registration, when a monitor is configured.
    ///
    /// When present, [`register_monitor`] registers with the monitor. A crash then dumps
    /// out-of-process, falling back to the in-process dump only if the monitor does not
    /// acknowledge.
    pub monitor: Option<(SocketAddr, Registration)>,
}

/// Prepares what the crash filter needs to produce a record, and registers with the monitor.
///
/// This is idempotent. A second call is a no-op. It names the record and dump files, reserves
/// handler stack, registers with the monitor, and registers the vectored handler. It opens a
/// socket, so it must not run under the loader lock. The filter itself comes from
/// [`install_filter`], and the module table from [`capture_modules`].
///
/// # Arguments
///
/// * `options` - where to write artifacts, the process name, and the full-memory flag.
///
/// # Returns
///
/// `true` when the handler is ready to produce records.
pub fn register_monitor(options: InstallOptions) -> bool {
    if STATE.get().is_some() {
        return true;
    }

    let _ = std::fs::create_dir_all(&options.directory);

    let stem = super::monitor::incident_stem(&options.process_name, std::process::id());
    let record_path = options.directory.join(format!("{stem}.record.txt"));
    let dump_path = options.directory.join(format!("{stem}.dmp"));

    let mut guarantee = HANDLER_STACK_GUARANTEE;
    unsafe { SetThreadStackGuarantee(&mut guarantee) };

    // Register with the out-of-process monitor before arming the handler. Best-effort. The monitor
    // reuses our stem, so its dump/report/modules files cluster with the record file named above.
    let monitor = options.monitor.and_then(|(address, mut registration)| {
        registration.stem = stem.clone();
        match super::monitor::register(address, &registration) {
            Ok(channel) => Some(channel),
            Err(error) => {
                // Not fatal, but it decides whether a crash is dumped from outside, so a default
                // run has to show it.
                tracing::warn!(
                    %error,
                    %address,
                    "crash handler: the monitor took no registration, so a crash is dumped in-process"
                );
                None
            }
        }
    });
    if monitor.is_some() {
        tracing::debug!("crash handler: registered with the out-of-process monitor");
    }

    let veh = unsafe { AddVectoredExceptionHandler(1, Some(vectored_handler)) };
    VEH_HANDLE.store(veh as usize, Ordering::SeqCst);

    let _ = STATE.set(CrashState {
        record_path: string_to_u16_buffer(record_path.to_string_lossy()),
        dump_path: string_to_u16_buffer(dump_path.to_string_lossy()),
        monitor,
        full_memory: options.full_memory,
    });

    true
}

/// Loads what the crash handler needs only once a crash happens, and captures the module table.
///
/// For after the layer reported ready: nothing here is needed to start the target. Pre-loads
/// `dbghelp` (from System32 only) so the in-process dump never loads it from the handler.
///
/// # Returns
///
/// The process's module table, captured once and shared with the handler, for the caller to log.
pub fn capture_modules() -> &'static ModuleTable {
    MODULES.get_or_init(|| {
        crate::process::load_system_library("dbghelp.dll");
        ModuleTable::capture()
    })
}

/// Removes the crash handler. Called on `DLL_PROCESS_DETACH`.
///
/// The handlers are always removed; the code is about to be unmapped. The clean-shutdown signal is
/// sent ONLY when the process is actually terminating. On a plain `FreeLibrary` unload the process
/// keeps running, so a later abnormal death must still be caught — signalling clean there would
/// blind the monitor to it.
///
/// # Arguments
///
/// * `process_terminating` - whether the process is exiting (the detach `lpReserved` was non-null),
///   as opposed to a `FreeLibrary` unload.
pub fn uninstall(process_terminating: bool) {
    if process_terminating
        && !INIT_FAILED.load(Ordering::SeqCst)
        && let Some(state) = STATE.get()
        && let Some(channel) = &state.monitor
    {
        channel.signal_clean_shutdown();
    }

    // The vectored handler must go; it fires for every exception and would dangle once the code is
    // unmapped. The unhandled filter is left as-is: on termination it is moot, and the layer is not
    // unloaded mid-run, so there is no live caller to dangle into.
    let veh = VEH_HANDLE.swap(0, Ordering::SeqCst);
    if veh != 0 {
        unsafe { RemoveVectoredExceptionHandler(veh as *mut c_void) };
    }
}

/// Tells the out-of-process monitor that layer initialization failed.
///
/// The layer calls this from its init-failure path (e.g. a `for_child` miss when the parent died
/// and the init event was deleted before this child could open it) before exiting. Without it, the
/// exit runs `DLL_PROCESS_DETACH`, which signals a clean shutdown, so the monitor would never learn
/// the layer failed to start. A no-op when no monitor is configured.
///
/// # Arguments
///
/// * `reason` - a human-readable cause, surfaced in the report.
///
/// # Returns
///
/// `true` when a registered channel delivered it. `false` before registration, or when the
/// monitor did not acknowledge in time; the caller then has to report another way.
pub fn signal_init_failure(reason: &str) -> bool {
    // Record the failure first: even if the signal below races the process exit and the monitor
    // misses it, `uninstall` will now decline to signal a clean shutdown, so the death is still
    // caught (as a termination) rather than mistaken for a normal close.
    INIT_FAILED.store(true, Ordering::SeqCst);
    STATE
        .get()
        .and_then(|state| state.monitor.as_ref())
        .is_some_and(|channel| channel.signal_init_failure(reason))
}

/// Reserves crash-handler stack on the current thread, once the handler is installed.
///
/// `SetThreadStackGuarantee` is per-thread, so the layer calls this on every `DLL_THREAD_ATTACH`:
/// without it, a stack overflow on a worker thread that never got the guarantee re-faults inside
/// the handler and bails via the re-entrancy guard. A no-op until the handler is installed. Note
/// this cannot cover the process's pre-existing main thread, which never gets a thread-attach
/// callback.
pub fn reserve_handler_stack() {
    if STATE.get().is_none() {
        return;
    }
    let mut guarantee = HANDLER_STACK_GUARANTEE;
    unsafe { SetThreadStackGuarantee(&mut guarantee) };
}

/// The unhandled-exception filter. The primary path for faults no one else handled.
unsafe extern "system" fn unhandled_filter(info: *mut EXCEPTION_POINTERS) -> LONG {
    // Only act on the native faults we own. A managed (.NET, `0xE0434352`) or C++ EH (`0xE06D7363`)
    // exception reaching the top-level filter is the runtime's to report, not ours — recording it
    // and dumping would turn an ordinary application error into a bogus mirrord crash report.
    if is_native_fault(unsafe { exception_code(info) }) {
        unsafe { handle_crash(info) };
    }

    // Chain to whatever the latest filter is so WER and any host handler still run.
    if let Some(previous) = filter_from_usize(PREVIOUS_FILTER.load(Ordering::SeqCst)) {
        return unsafe { previous(info) };
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// The vectored handler. A narrow first-chance net.
unsafe extern "system" fn vectored_handler(info: *mut EXCEPTION_POINTERS) -> LONG {
    let code = unsafe { exception_code(info) };

    if code == EXCEPTION_STACK_OVERFLOW {
        // First-chance is already fatal for a stack overflow. No false positive is possible.
        unsafe { handle_crash(info) };
    } else if is_severe(code) && unsafe { IsDebuggerPresent() } == 0 {
        // Log severe first-chance faults, but stay quiet under a debugger: the developer already
        // sees them there, and this only ever returns EXCEPTION_CONTINUE_SEARCH, so it never alters
        // the exception's path either way.
        unsafe { log_first_chance(code) };
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// Produces the crash record, stderr stub, and dump. Runs at most once per process.
unsafe fn handle_crash(info: *mut EXCEPTION_POINTERS) {
    // This runs on the faulting thread, which is one of the target's. The report below is
    // written with `CreateFileW` and `WriteFile`, and in an injected process those are hooked:
    // without this marker the write goes to the agent, and a proxy round-trip inside an
    // exception handler is the opposite of the allocation-free local write this module
    // promises. Outside an injected process the marker costs one thread-local store.
    let _internal = crate::internal_thread::InternalGuard::enter();

    // The filter is in place before `register_monitor` has run. An exception in that window
    // has nothing to be recorded with, and a filter further down the chain may still recover
    // from it, so it must not use up the one record this process gets.
    let Some(state) = STATE.get() else {
        return;
    };

    // A fault inside the handler re-enters here. Bail rather than loop.
    if IN_HANDLER.swap(true, Ordering::SeqCst) {
        return;
    }

    // Both registrations may fire for the same crash. Produce the record only once.
    if HANDLED.swap(true, Ordering::SeqCst) {
        IN_HANDLER.store(false, Ordering::SeqCst);
        return;
    }

    unsafe {
        let recorded = write_crash_record(state, info);
        write_stderr_stub(info, recorded);

        // Prefer the out-of-process dump. It reads the crashed process from the outside, which
        // is the safe model. Fall back to the in-process dump only if the monitor is absent or
        // does not acknowledge.
        if !signal_monitor(state, info) {
            write_in_process_dump(state, info);
        }
    }

    IN_HANDLER.store(false, Ordering::SeqCst);
}

/// Writes the text crash record. Opens the file lazily, so no record is left for a clean exit.
/// Allocation-free.
///
/// # Returns
///
/// `true` when the whole record reached the file.
unsafe fn write_crash_record(state: &CrashState, info: *mut EXCEPTION_POINTERS) -> bool {
    let file = unsafe { open_truncating(&state.record_path) };
    if file == INVALID_HANDLE_VALUE {
        return false;
    }

    let mut storage = [0u8; 4096];
    let mut record = FixedBuf::new(&mut storage);

    let exception = unsafe { (*info).ExceptionRecord };
    let code = unsafe { (*exception).ExceptionCode };
    let address = unsafe { (*exception).ExceptionAddress } as usize;

    let _ = writeln!(record, "mirrord layer crash");
    let _ = writeln!(
        record,
        "pid: {}  tid: {}",
        unsafe { GetCurrentProcessId() },
        unsafe { GetCurrentThreadId() },
    );
    let _ = writeln!(record, "code: {code:#010x} ({})", code_name(code));
    let _ = writeln!(record, "address: {address:#018x}");

    // Access violations carry the operation and the faulting address.
    if code == EXCEPTION_ACCESS_VIOLATION && unsafe { (*exception).NumberParameters } >= 2 {
        let operation = match unsafe { (*exception).ExceptionInformation[0] } {
            0 => "read",
            1 => "write",
            8 => "execute",
            _ => "?",
        };
        let faulting = unsafe { (*exception).ExceptionInformation[1] };
        let _ = writeln!(record, "access: {operation} va={faulting:#018x}");
    }

    match MODULES
        .get()
        .and_then(|modules| modules.resolve_offset(address))
    {
        Some((module, offset)) => {
            let _ = writeln!(record, "faulting module: {module}+{offset:#x}");
        }
        None => {
            let _ = writeln!(record, "faulting module: unknown");
        }
    }

    let mut frames = [std::ptr::null_mut::<c_void>(); STACK_FRAMES];
    let captured = unsafe {
        RtlCaptureStackBackTrace(
            0,
            STACK_FRAMES as DWORD,
            frames.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    let _ = writeln!(record, "stack:");
    for frame in frames.iter().take(captured as usize) {
        let frame = *frame as usize;
        match MODULES
            .get()
            .and_then(|modules| modules.resolve_offset(frame))
        {
            Some((module, offset)) => {
                let _ = writeln!(record, "  {frame:#018x} {module}+{offset:#x}");
            }
            None => {
                let _ = writeln!(record, "  {frame:#018x}");
            }
        }
    }

    unsafe {
        let written = write_all(file, record.filled());
        CloseHandle(file);
        written
    }
}

/// Writes a one-line crash notice to stderr so the user sees something even without the monitor.
///
/// # Arguments
///
/// * `recorded` - whether [`write_crash_record`] wrote the record, so the notice never points at a
///   file that does not exist.
unsafe fn write_stderr_stub(info: *mut EXCEPTION_POINTERS, recorded: bool) {
    let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if stderr.is_null() || stderr == INVALID_HANDLE_VALUE {
        return;
    }

    let exception = unsafe { (*info).ExceptionRecord };
    let code = unsafe { (*exception).ExceptionCode };
    let address = unsafe { (*exception).ExceptionAddress } as usize;
    let module = MODULES
        .get()
        .and_then(|modules| modules.resolve(address))
        .unwrap_or("unknown");

    let mut storage = [0u8; 256];
    let mut line = FixedBuf::new(&mut storage);
    let _ = if recorded {
        writeln!(
            line,
            "mirrord: crashed ({code:#010x}) in {module}; crash record written next to the layer log",
        )
    } else {
        writeln!(
            line,
            "mirrord: crashed ({code:#010x}) in {module}; the crash record could not be written",
        )
    };
    unsafe { write_all(stderr as HANDLE, line.filled()) };
}

/// Signals the out-of-process monitor and waits for it to finish the dump.
///
/// # Returns
///
/// `true` when a monitor is configured and acknowledged the dump.
unsafe fn signal_monitor(state: &CrashState, info: *mut EXCEPTION_POINTERS) -> bool {
    match &state.monitor {
        Some(channel) => {
            let thread_id = unsafe { GetCurrentThreadId() };
            channel.signal_crash(thread_id, info as usize)
        }
        None => false,
    }
}

/// Writes a minidump of the crashing process to the pre-named dump file.
unsafe fn write_in_process_dump(state: &CrashState, info: *mut EXCEPTION_POINTERS) {
    let file = unsafe { open_truncating(&state.dump_path) };
    if file == INVALID_HANDLE_VALUE {
        return;
    }

    let mut exception = MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: unsafe { GetCurrentThreadId() },
        ExceptionPointers: info as *mut _,
        ClientPointers: 0,
    };
    super::dump::write_dump(
        unsafe { GetCurrentProcess() } as RawHandle,
        unsafe { GetCurrentProcessId() },
        file as RawHandle,
        Some(&mut exception),
        state.full_memory,
    );

    unsafe { CloseHandle(file) };
}

/// Logs a severe first-chance exception to stderr.
unsafe fn log_first_chance(code: DWORD) {
    let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if stderr.is_null() || stderr == INVALID_HANDLE_VALUE {
        return;
    }

    let mut storage = [0u8; 128];
    let mut line = FixedBuf::new(&mut storage);
    let _ = writeln!(
        line,
        "mirrord: first-chance {code:#010x} ({})",
        code_name(code)
    );
    unsafe { write_all(stderr as HANDLE, line.filled()) };
}

/// Opens a null-terminated wide path for writing, truncating any existing content.
///
/// # Safety
///
/// `path` must be a null-terminated wide string.
unsafe fn open_truncating(path: &[u16]) -> HANDLE {
    unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ,
            std::ptr::null_mut(),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    }
}

/// Writes a whole buffer with raw `WriteFile`. Allocation-free.
///
/// # Returns
///
/// `true` when every byte was written.
unsafe fn write_all(handle: HANDLE, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let mut written: DWORD = 0;
        let ok = unsafe {
            WriteFile(
                handle,
                bytes.as_ptr() as *const c_void,
                bytes.len() as DWORD,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == FALSE || written == 0 {
            return false;
        }
        bytes = bytes.get(written as usize..).unwrap_or(&[]);
    }
    true
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, atomic::AtomicU32};

    use winapi::um::winnt::EXCEPTION_RECORD;

    use super::*;

    /// The OS slot and the chain target are process-wide, so the tests that touch them take turns.
    static FILTER_SLOT: Mutex<()> = Mutex::new(());

    static EARLY_CALLS: AtomicU32 = AtomicU32::new(0);
    static LATE_CALLS: AtomicU32 = AtomicU32::new(0);

    /// A filter the target registered before the layer loaded, from a static initializer say.
    unsafe extern "system" fn early_filter(_: *mut EXCEPTION_POINTERS) -> LONG {
        EARLY_CALLS.fetch_add(1, Ordering::SeqCst);
        EXCEPTION_CONTINUE_SEARCH
    }

    /// A filter the target's runtime registers once the hooks are live.
    unsafe extern "system" fn late_filter(_: *mut EXCEPTION_POINTERS) -> LONG {
        LATE_CALLS.fetch_add(1, Ordering::SeqCst);
        EXCEPTION_CONTINUE_SEARCH
    }

    /// Runs the installed filter the way the OS would, with a C++ exception, which the filter
    /// never records, so only the chaining is exercised.
    fn run_filter() -> LONG {
        run_filter_with(0xE06D_7363)
    }

    /// Runs the installed filter the way the OS would, with an exception of `code`.
    fn run_filter_with(code: DWORD) -> LONG {
        let mut record: EXCEPTION_RECORD = unsafe { std::mem::zeroed() };
        record.ExceptionCode = code;
        let mut pointers = EXCEPTION_POINTERS {
            ExceptionRecord: &mut record,
            ContextRecord: std::ptr::null_mut(),
        };
        unsafe { unhandled_filter(&mut pointers) }
    }

    /// Reads the OS slot without changing it.
    fn os_slot() -> usize {
        let current = unsafe { SetUnhandledExceptionFilter(None) };
        unsafe { SetUnhandledExceptionFilter(current) };
        filter_to_usize(current)
    }

    /// The filter has to reach the OS, and every filter the target registers, before or after it,
    /// has to stay in the chain.
    #[test]
    fn the_filter_reaches_the_os_and_chains_to_the_targets_filters() {
        let _slot = FILTER_SLOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = unsafe { SetUnhandledExceptionFilter(Some(early_filter)) };
        let early_calls = EARLY_CALLS.load(Ordering::SeqCst);
        let late_calls = LATE_CALLS.load(Ordering::SeqCst);

        install_filter();
        assert_eq!(
            os_slot(),
            filter_to_usize(Some(unhandled_filter)),
            "the OS slot must hold mirrord's filter"
        );

        run_filter();
        assert_eq!(
            EARLY_CALLS.load(Ordering::SeqCst),
            early_calls + 1,
            "chains to the earlier filter"
        );

        // What the `SetUnhandledExceptionFilter` hook does with a later registration.
        let displaced = adopt_previous_filter(Some(late_filter));
        assert_eq!(
            filter_to_usize(displaced),
            filter_to_usize(Some(early_filter))
        );
        assert_eq!(
            os_slot(),
            filter_to_usize(Some(unhandled_filter)),
            "a later registration leaves ours"
        );

        run_filter();
        assert_eq!(
            LATE_CALLS.load(Ordering::SeqCst),
            late_calls + 1,
            "chains to the later filter"
        );
        assert_eq!(
            EARLY_CALLS.load(Ordering::SeqCst),
            early_calls + 1,
            "and no longer to the earlier one"
        );

        // A layer that fails after installing gives the slot to the target's latest filter.
        restore_filter();
        assert_eq!(os_slot(), filter_to_usize(Some(late_filter)));

        unsafe { SetUnhandledExceptionFilter(original) };
    }

    /// A native fault that reaches the filter before `register_monitor` has run, and that a
    /// chained filter may still recover from, must not use up the one record the process gets.
    #[test]
    fn a_fault_before_registration_does_not_use_up_the_record() {
        let _slot = FILTER_SLOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = unsafe { SetUnhandledExceptionFilter(None) };
        install_filter();
        let displaced = adopt_previous_filter(Some(early_filter));
        let early_calls = EARLY_CALLS.load(Ordering::SeqCst);

        assert!(STATE.get().is_none(), "no test registers with a monitor");
        run_filter_with(EXCEPTION_ACCESS_VIOLATION);

        assert!(
            !HANDLED.load(Ordering::SeqCst),
            "a fault with nothing to record it with must not claim the record"
        );
        assert_eq!(
            EARLY_CALLS.load(Ordering::SeqCst),
            early_calls + 1,
            "the fault still reaches the target's filter"
        );

        adopt_previous_filter(displaced);
        restore_filter();
        unsafe { SetUnhandledExceptionFilter(original) };
    }
}

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
//! ## Out-of-process dumping
//!
//! The safe dump is the monitor's job: dumping a crashed process from the outside avoids running
//! dump code in a broken address space. When [`install`] registered with a monitor, a crash signals
//! it and waits for the out-of-process dump. The in-process dump here is the fallback, used only
//! when there is no monitor or it does not acknowledge in time.

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
    /// Cached module table for loader-free address resolution.
    modules: ModuleTable,
    /// The out-of-process dump channel, when a monitor is configured.
    monitor: Option<MonitorChannel>,
    /// Whether to upgrade the dump to full memory.
    full_memory: bool,
}

static STATE: OnceLock<CrashState> = OnceLock::new();
static VEH_HANDLE: AtomicUsize = AtomicUsize::new(0);
/// The filter our handler chains to, as a `usize`.
///
/// It starts as whatever filter was installed before us. The layer's `SetUnhandledExceptionFilter`
/// hook updates it when the target's runtime tries to install its own filter, so we still chain to
/// the latest while keeping ours as the OS top-level filter.
static PREVIOUS_FILTER: AtomicUsize = AtomicUsize::new(0);
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

/// Options for [`install`].
pub struct InstallOptions {
    /// Directory for crash artifacts. Usually the layer log directory.
    pub directory: PathBuf,
    /// Process name used in artifact file names.
    pub process_name: String,
    /// Whether to upgrade the minidump to a full-memory dump.
    pub full_memory: bool,
    /// The crash monitor endpoint and this process's registration, when a monitor is configured.
    ///
    /// When present, [`install`] registers with the monitor. A crash then dumps out-of-process,
    /// falling back to the in-process dump only if the monitor does not acknowledge.
    pub monitor: Option<(SocketAddr, Registration)>,
}

/// Installs the crash handler.
///
/// This is idempotent. A second call is a no-op. It pre-opens the record file, reserves handler
/// stack, pre-loads `dbghelp`, captures the module table, and registers the handlers.
///
/// # Arguments
///
/// * `options` - where to write artifacts, the process name, and the full-memory flag.
///
/// # Returns
///
/// `true` when the handler is installed.
pub fn install(options: InstallOptions) -> bool {
    if STATE.get().is_some() {
        return true;
    }

    let _ = std::fs::create_dir_all(&options.directory);

    let stem = format!(
        "mirrord-crash_{}_{}_pid{}",
        super::timestamp(),
        super::sanitize_name(&options.process_name),
        std::process::id(),
    );
    let record_path = options.directory.join(format!("{stem}.record.txt"));
    let dump_path = options.directory.join(format!("{stem}.dmp"));

    let mut guarantee = HANDLER_STACK_GUARANTEE;
    unsafe { SetThreadStackGuarantee(&mut guarantee) };

    // Pre-load dbghelp (from System32 only) so the in-process dump never loads it from the handler.
    crate::process::load_system_library("dbghelp.dll");

    let modules = ModuleTable::capture();

    // Register with the out-of-process monitor before arming the handler. Best-effort. The monitor
    // reuses our stem, so its dump/report/modules files cluster with the record file named above.
    let monitor = options.monitor.and_then(|(address, mut registration)| {
        registration.stem = stem.clone();
        super::monitor::register(address, &registration)
    });
    if monitor.is_some() {
        tracing::debug!("crash handler: registered with the out-of-process monitor");
    }

    // Install our filter, then remember whatever was there as the chain target. The layer hooks
    // `SetUnhandledExceptionFilter` so a later override by the target's runtime cannot clobber
    // ours.
    let previous = unsafe { SetUnhandledExceptionFilter(Some(unhandled_filter)) };
    PREVIOUS_FILTER.store(filter_to_usize(previous), Ordering::SeqCst);

    let veh = unsafe { AddVectoredExceptionHandler(1, Some(vectored_handler)) };
    VEH_HANDLE.store(veh as usize, Ordering::SeqCst);

    let _ = STATE.set(CrashState {
        record_path: string_to_u16_buffer(record_path.to_string_lossy()),
        dump_path: string_to_u16_buffer(dump_path.to_string_lossy()),
        modules,
        monitor,
        full_memory: options.full_memory,
    });

    true
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
pub fn signal_init_failure(reason: &str) {
    // Record the failure first: even if the signal below races the process exit and the monitor
    // misses it, `uninstall` will now decline to signal a clean shutdown, so the death is still
    // caught (as a termination) rather than mistaken for a normal close.
    INIT_FAILED.store(true, Ordering::SeqCst);
    if let Some(state) = STATE.get()
        && let Some(channel) = &state.monitor
    {
        channel.signal_init_failure(reason);
    }
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
    // A fault inside the handler re-enters here. Bail rather than loop.
    if IN_HANDLER.swap(true, Ordering::SeqCst) {
        return;
    }

    // Both registrations may fire for the same crash. Produce the record only once.
    if HANDLED.swap(true, Ordering::SeqCst) {
        IN_HANDLER.store(false, Ordering::SeqCst);
        return;
    }

    if let Some(state) = STATE.get() {
        unsafe {
            write_crash_record(state, info);
            write_stderr_stub(state, info);

            // Prefer the out-of-process dump. It reads the crashed process from the outside, which
            // is the safe model. Fall back to the in-process dump only if the monitor is absent or
            // does not acknowledge.
            if !signal_monitor(state, info) {
                write_in_process_dump(state, info);
            }
        }
    }

    IN_HANDLER.store(false, Ordering::SeqCst);
}

/// Writes the text crash record. Opens the file lazily, so no record is left for a clean exit.
/// Allocation-free.
unsafe fn write_crash_record(state: &CrashState, info: *mut EXCEPTION_POINTERS) {
    let file = unsafe { open_truncating(&state.record_path) };
    if file == INVALID_HANDLE_VALUE {
        return;
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

    match state.modules.resolve_offset(address) {
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
        match state.modules.resolve_offset(frame) {
            Some((module, offset)) => {
                let _ = writeln!(record, "  {frame:#018x} {module}+{offset:#x}");
            }
            None => {
                let _ = writeln!(record, "  {frame:#018x}");
            }
        }
    }

    unsafe {
        write_all(file, record.filled());
        CloseHandle(file);
    }
}

/// Writes a one-line crash notice to stderr so the user sees something even without the monitor.
unsafe fn write_stderr_stub(state: &CrashState, info: *mut EXCEPTION_POINTERS) {
    let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if stderr.is_null() || stderr == INVALID_HANDLE_VALUE {
        return;
    }

    let exception = unsafe { (*info).ExceptionRecord };
    let code = unsafe { (*exception).ExceptionCode };
    let address = unsafe { (*exception).ExceptionAddress } as usize;
    let module = state.modules.resolve(address).unwrap_or("unknown");

    let mut storage = [0u8; 256];
    let mut line = FixedBuf::new(&mut storage);
    let _ = writeln!(
        line,
        "mirrord: crashed ({code:#010x}) in {module}; crash record written next to the layer log",
    );
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
unsafe fn write_all(handle: HANDLE, mut bytes: &[u8]) {
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
            break;
        }
        bytes = bytes.get(written as usize..).unwrap_or(&[]);
    }
}

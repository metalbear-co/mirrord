//! The server side of the monitor — the accept-and-watch loop the CLI runs.
//!
//! [`serve`] binds nothing itself; it takes a listener and blocks, accepting registrations. Each
//! registration becomes a [`PidWatch`] running on its own thread, which waits on the process and
//! its crash event: a crash signal triggers an out-of-process dump and a report, while a death with
//! no signal is classified (clean shutdown, debugger stop, application-level exit, or external
//! kill).

use std::{
    io::{self, Write},
    net::{TcpListener, TcpStream},
    os::windows::io::RawHandle,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use str_win::string_to_u16_buffer;
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, FALSE, TRUE},
        ntdef::HANDLE,
    },
    um::{
        debugapi::CheckRemoteDebuggerPresent,
        fileapi::{CREATE_ALWAYS, CreateFileW},
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        memoryapi::{CreateFileMappingW, FILE_MAP_ALL_ACCESS, MapViewOfFile},
        processthreadsapi::{GetExitCodeProcess, OpenProcess},
        synchapi::{CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject},
        winbase::{INFINITE, WAIT_OBJECT_0},
        winnt::{
            FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GENERIC_WRITE, PAGE_READWRITE,
            PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, SYNCHRONIZE,
        },
    },
};
use windows_sys::Win32::System::Diagnostics::Debug::MINIDUMP_EXCEPTION_INFORMATION;

use super::{
    ACK_FAILED, ACK_READY, CLEAN_EVENT_PREFIX, CRASH_EVENT_PREFIX, CrashInfo, DONE_EVENT_PREFIX,
    INFO_SECTION_PREFIX, KIND_INIT_FAILURE, Registration, read_reason, read_registration,
};
use crate::diagnostics::{
    dump,
    handle::OwnedHandle,
    report::{self, CrashReport, Outcome, ProcessNode},
};

/// The monitor's output policy: where artifacts go and what dumps to capture.
///
/// Set once by the CLI and shared by every watcher thread (behind an `Arc`), so a watch never
/// carries a long argument list for the same handful of session-wide settings.
pub struct MonitorConfig {
    /// The mirrord version string, for the report.
    pub version: String,
    /// Directory the crash artifacts are written to.
    pub dump_directory: PathBuf,
    /// Whether to upgrade the always-captured minidump to a full-memory dump.
    pub full_memory: bool,
    /// Whether `dump_directory` is a session temp dir to remove on a clean exit.
    pub ephemeral: bool,
}

/// Shared session state the reporter reads to build the process tree.
///
/// Every registration is recorded here. Each watcher thread reads a snapshot to compose its report.
struct Registry {
    /// The root CLI process id.
    root_pid: u32,
    /// Every registered process this session.
    nodes: Vec<ProcessNode>,
    /// Whether a crash popup has already been shown. Coalesces to one per session.
    popup_shown: bool,
    /// Whether any crash was surfaced this session. Keeps an ephemeral session dir from being
    /// cleaned up when something actually crashed.
    reported: bool,
}

/// Runs the monitor server loop on a bound listener.
///
/// This blocks. It accepts registrations and spawns a watcher thread per registered pid. The CLI
/// runs it on a blocking task.
///
/// # Arguments
///
/// * `listener` - the bound registration socket.
/// * `root_pid` - the root CLI process id, for the process tree and the lifetime watchdog.
/// * `config` - the session-wide output policy.
///
/// # Returns
///
/// `Ok` when the accept loop ends.
pub fn serve(listener: TcpListener, root_pid: u32, config: MonitorConfig) -> io::Result<()> {
    tracing::info!(
        root_pid,
        directory = %config.dump_directory.display(),
        ephemeral = config.ephemeral,
        "crash monitor listening",
    );

    let config = Arc::new(config);
    let registry = Arc::new(Mutex::new(Registry {
        root_pid,
        nodes: Vec::new(),
        popup_shown: false,
        reported: false,
    }));

    // Exit when the root CLI dies, so the monitor never outlives its session even if the CLI exits
    // via `exec` without running destructors. The watchdog also cleans up an ephemeral session dir.
    {
        let registry = Arc::clone(&registry);
        let config = Arc::clone(&config);
        std::thread::spawn(move || watch_root(root_pid, registry, config));
    }

    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                if let Err(error) = accept(&mut stream, &config, &registry) {
                    tracing::warn!(%error, "crash monitor: failed to accept a registration");
                }
            }
            Err(error) => tracing::warn!(%error, "crash monitor: accept error"),
        }
    }

    Ok(())
}

/// Handles one registration: record it, prepare the objects, ack, and spawn the watcher.
fn accept(
    stream: &mut TcpStream,
    config: &Arc<MonitorConfig>,
    registry: &Arc<Mutex<Registry>>,
) -> io::Result<()> {
    // A client that connects but stalls before writing must not wedge the whole accept loop.
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;

    let registration = read_registration(stream)?;
    tracing::info!(
        pid = registration.pid,
        parent_pid = registration.parent_pid,
        name = %registration.name,
        role = %registration.role,
        "crash monitor: registration",
    );

    if let Ok(mut registry) = registry.lock() {
        registry.nodes.push(ProcessNode {
            pid: registration.pid,
            parent_pid: registration.parent_pid,
            name: registration.name.clone(),
            role: registration.role.clone(),
            log_path: registration.log_path.clone().map(PathBuf::from),
            exit_code: None,
        });
    }

    let watch = prepare(&registration);
    let ack = if watch.is_some() {
        ACK_READY
    } else {
        ACK_FAILED
    };
    stream.write_all(&[ack])?;
    stream.flush()?;

    if let Some(watch) = watch {
        let config = Arc::clone(config);
        let registry = Arc::clone(registry);
        std::thread::spawn(move || watch.run(config, registry));
    }

    Ok(())
}

/// A registered pid the monitor watches. Holds the per-pid objects.
///
/// Each handle and the mapped view are an [`OwnedHandle`], so the watch frees everything by
/// dropping — there is no teardown method and the watcher thread can take the value by value.
struct PidWatch {
    pid: u32,
    parent_pid: u32,
    name: String,
    /// The shared incident stem, used to name the dump/report/zip and to find the in-proc record.
    stem: String,
    /// Whether a debugger was attached when this process registered. An intentional debugger stop
    /// (`TerminateProcess`) is then not reported as a crash or kill.
    debugged: bool,
    /// Handle to the watched process, for the wait, the exit code, and the out-of-process dump.
    process: OwnedHandle,
    /// Auto-reset event the layer sets to signal a crash or init failure; the monitor waits on it.
    crash_event: OwnedHandle,
    /// Auto-reset event the monitor sets once it has handled the signal; the layer waits on it.
    done_event: OwnedHandle,
    /// Manual-reset event the layer sets on a clean shutdown; read after death to classify it.
    clean_event: OwnedHandle,
    /// The shared-section mapping handle, held so the mapped `view` stays valid.
    _mapping: OwnedHandle,
    /// A mapped view of the shared `CrashInfo` section the layer writes the crash facts into.
    view: OwnedHandle,
}

/// Opens a process handle and creates the per-pid crash objects for a registration.
fn prepare(registration: &Registration) -> Option<PidWatch> {
    let pid = registration.pid;
    let crash_name = string_to_u16_buffer(format!("{CRASH_EVENT_PREFIX}{pid}"));
    let done_name = string_to_u16_buffer(format!("{DONE_EVENT_PREFIX}{pid}"));
    let clean_name = string_to_u16_buffer(format!("{CLEAN_EVENT_PREFIX}{pid}"));
    let info_name = string_to_u16_buffer(format!("{INFO_SECTION_PREFIX}{pid}"));

    unsafe {
        let process = OwnedHandle::kernel(OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | SYNCHRONIZE,
            FALSE,
            pid,
        ));
        let Some(process) = process else {
            tracing::warn!(pid, "crash monitor: OpenProcess failed");
            return None;
        };

        // Note a debugger now, while the process is alive. A later debugger "stop" terminates it
        // with no clean shutdown; this lets the death-detector tell that intentional stop apart
        // from a real external kill.
        let mut debugger: BOOL = FALSE;
        if CheckRemoteDebuggerPresent(process.as_ptr(), &mut debugger) == 0 {
            debugger = FALSE;
        }

        // As in `open_channel`, the `?` cascade frees whatever opened before a failure as the
        // locals (including `process` above) drop — no hand-written close cascade.
        let crash_event = OwnedHandle::kernel(CreateEventW(
            std::ptr::null_mut(),
            FALSE,
            FALSE,
            crash_name.as_ptr(),
        ))?;
        let done_event = OwnedHandle::kernel(CreateEventW(
            std::ptr::null_mut(),
            FALSE,
            FALSE,
            done_name.as_ptr(),
        ))?;
        // Manual-reset so the monitor can read its state once the process has died.
        let clean_event = OwnedHandle::kernel(CreateEventW(
            std::ptr::null_mut(),
            TRUE,
            FALSE,
            clean_name.as_ptr(),
        ))?;
        let mapping = OwnedHandle::kernel(CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null_mut(),
            PAGE_READWRITE,
            0,
            std::mem::size_of::<CrashInfo>() as DWORD,
            info_name.as_ptr(),
        ))?;

        let view = OwnedHandle::view(MapViewOfFile(
            mapping.as_ptr(),
            FILE_MAP_ALL_ACCESS,
            0,
            0,
            std::mem::size_of::<CrashInfo>(),
        ))?;

        Some(PidWatch {
            pid,
            parent_pid: registration.parent_pid,
            name: registration.name.clone(),
            stem: registration.stem.clone(),
            debugged: debugger != FALSE,
            process,
            crash_event,
            done_event,
            clean_event,
            _mapping: mapping,
            view,
        })
    }
}

/// How the monitor classifies a registered process's death when no crash signal fired.
///
/// The precedence is fixed: a clean-shutdown signal wins over a debugger stop, which wins over an
/// application-level exit code; only a death with none of those is a reportable kill.
#[derive(Debug, PartialEq, Eq)]
enum DeathVerdict {
    /// The layer signalled a clean shutdown — a normal close at any exit code.
    CleanShutdown,
    /// The process was being debugged — an intentional developer stop, not a fault.
    DebuggerStop,
    /// The exit code is a runtime-defined exception or a Ctrl-C the runtime reports itself.
    ApplicationLevel,
    /// No clean signal, no debugger, no application-level code: an external kill to report.
    Reportable,
}

/// Classifies a death from the three signals the watcher gathered.
///
/// Pure, so the precedence is unit-testable away from live Win32 state. `run` only gathers the
/// inputs and dispatches the logging and reporting.
///
/// # Arguments
///
/// * `clean` - whether the layer set the clean-shutdown event before dying.
/// * `debugged` - whether a debugger was attached when the process registered.
/// * `exit_code` - the process exit code.
///
/// # Returns
///
/// The [`DeathVerdict`] for the death.
fn classify_death(clean: bool, debugged: bool, exit_code: u32) -> DeathVerdict {
    if clean {
        DeathVerdict::CleanShutdown
    } else if debugged {
        DeathVerdict::DebuggerStop
    } else if report::is_app_level_exit(exit_code) {
        DeathVerdict::ApplicationLevel
    } else {
        DeathVerdict::Reportable
    }
}

/// Records a registered process's death in the registry, so it stays in the process tree marked
/// dead. The node is never removed — the session keeps every process it saw.
fn mark_dead(registry: &Mutex<Registry>, pid: u32, exit_code: u32) {
    if let Ok(mut registry) = registry.lock()
        && let Some(node) = registry.nodes.iter_mut().find(|node| node.pid == pid)
    {
        node.exit_code = Some(exit_code);
    }
}

impl PidWatch {
    /// Waits on the process and its crash event until the process exits.
    ///
    /// A crash event triggers an out-of-process dump and a report. The process handle signalling
    /// means it died; if it did so with no crash signal, that death is reported as a termination.
    fn run(self, config: Arc<MonitorConfig>, registry: Arc<Mutex<Registry>>) {
        let process = self.process.as_ptr() as HANDLE;
        let crash_event = self.crash_event.as_ptr() as HANDLE;
        let mut reported = false;

        loop {
            let handles = [process, crash_event];
            let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, INFINITE) };

            if result == WAIT_OBJECT_0 {
                let exit_code = self.exit_code();
                let exit = format!("{exit_code:#010x}");
                // Record the death so this process stays in the tree marked dead — whatever the
                // verdict — for any report composed after it (e.g. a child whose parent died).
                mark_dead(&registry, self.pid, exit_code);
                match classify_death(self.shut_down_cleanly(), self.debugged, exit_code) {
                    DeathVerdict::CleanShutdown => {
                        // The layer ran its detach, so this is a normal close at any exit code.
                        tracing::debug!(
                            pid = self.pid,
                            name = %self.name,
                            exit,
                            "crash monitor: layer process shut down cleanly",
                        );
                    }
                    DeathVerdict::DebuggerStop => {
                        // A debugger "stop" terminates the process with no clean shutdown. That is
                        // an intentional developer action (IDE-extension stop), not a mirrord
                        // fault.
                        tracing::debug!(
                            pid = self.pid,
                            name = %self.name,
                            exit,
                            "crash monitor: layer process terminated under a debugger (intentional stop)",
                        );
                    }
                    DeathVerdict::ApplicationLevel => {
                        // A managed-runtime exception or a Ctrl-C that did not run detach. The
                        // app's own business, reported by the runtime, not
                        // by us.
                        tracing::debug!(
                            pid = self.pid,
                            name = %self.name,
                            exit,
                            "crash monitor: layer process exited with an application-level code",
                        );
                    }
                    DeathVerdict::Reportable if !reported => {
                        // No clean-shutdown signal and no crash signal: killed or died abruptly.
                        tracing::warn!(
                            pid = self.pid,
                            parent_pid = self.parent_pid,
                            name = %self.name,
                            exit,
                            "crash monitor: layer process died without a clean shutdown",
                        );
                        self.report(
                            &registry,
                            &config,
                            Outcome::Terminated { exit_code },
                            None,
                            None,
                        );
                    }
                    DeathVerdict::Reportable => {}
                }
                break;
            } else if result == WAIT_OBJECT_0 + 1 {
                // The layer signalled through the shared section. The process is frozen on the
                // handshake, so enumerate its modules now, before acking.
                let info = unsafe { *(self.view.as_ptr() as *const CrashInfo) };
                let modules = crate::modules::module_inventory(process);
                let modules = (!modules.is_empty()).then_some(modules);

                if info.kind == KIND_INIT_FAILURE {
                    let reason = read_reason(&info);
                    unsafe { SetEvent(self.done_event.as_ptr() as HANDLE) };
                    if !reported {
                        self.report(
                            &registry,
                            &config,
                            Outcome::InitFailed { reason },
                            None,
                            modules.as_deref(),
                        );
                        reported = true;
                    }
                } else {
                    // The minidump is always captured; full-memory is the only gated part of it.
                    let dump = self.dump(&config);
                    unsafe { SetEvent(self.done_event.as_ptr() as HANDLE) };
                    if !reported {
                        self.report(
                            &registry,
                            &config,
                            Outcome::Crashed,
                            dump.as_deref(),
                            modules.as_deref(),
                        );
                        reported = true;
                    }
                }
            } else {
                tracing::warn!(
                    pid = self.pid,
                    result,
                    "crash monitor: unexpected wait result"
                );
                break;
            }
        }
    }

    /// Reads the process's exit code.
    fn exit_code(&self) -> u32 {
        let mut code: DWORD = 0;
        unsafe { GetExitCodeProcess(self.process.as_ptr() as HANDLE, &mut code) };
        code
    }

    /// Reports whether the layer signalled a clean shutdown before dying.
    ///
    /// This is the verdict the death classification rests on, not the exit code.
    fn shut_down_cleanly(&self) -> bool {
        unsafe { WaitForSingleObject(self.clean_event.as_ptr() as HANDLE, 0) == WAIT_OBJECT_0 }
    }

    /// Composes and surfaces the report from a registry snapshot.
    ///
    /// # Arguments
    ///
    /// * `registry` - the shared session state.
    /// * `config` - the session-wide output policy.
    /// * `outcome` - how the focused process ended.
    /// * `dump` - the minidump path, when one was produced.
    /// * `modules` - the crashing process's module inventory, when enumerated.
    fn report(
        &self,
        registry: &Mutex<Registry>,
        config: &MonitorConfig,
        outcome: Outcome,
        dump: Option<&Path>,
        modules: Option<&str>,
    ) {
        // Snapshot the registry and claim the single per-session dialog under a brief lock. The
        // dialog blocks until dismissed, so it must never be shown while the registry mutex is held
        // — that would stall every new layer registration for the whole session.
        let (nodes, root_pid, show_dialog) = {
            let Ok(mut registry) = registry.lock() else {
                return;
            };
            // First report of the session claims the single dialog; the rest are file-only. Every
            // report (and its zip) is written to disk regardless, so nothing is lost either way.
            let show_dialog = !registry.popup_shown;
            registry.popup_shown = true;
            registry.reported = true;
            (registry.nodes.clone(), registry.root_pid, show_dialog)
        };

        // The in-proc record shares the incident stem, so the path is known without a directory
        // scan.
        let record = {
            let path = config
                .dump_directory
                .join(format!("{}.record.txt", self.stem));
            path.exists().then_some(path)
        };

        // Bundle the exact registered log files, so a prior session's reused pid can never leak in.
        let logs: Vec<PathBuf> = nodes
            .iter()
            .filter_map(|node| node.log_path.clone())
            .collect();

        let report = CrashReport {
            focus_pid: self.pid,
            focus_name: &self.name,
            outcome,
            nodes: &nodes,
            root_pid,
            dump,
            record: record.as_deref(),
            logs: &logs,
            modules,
            mirrord_version: &config.version,
            stem: &self.stem,
        };
        report::surface(&report, &config.dump_directory, show_dialog);
    }

    /// Writes an out-of-process minidump using the shared crash facts.
    ///
    /// # Returns
    ///
    /// The dump path when the dump succeeded.
    fn dump(&self, config: &MonitorConfig) -> Option<PathBuf> {
        let info = unsafe { *(self.view.as_ptr() as *const CrashInfo) };

        let path = config.dump_directory.join(format!("{}.dmp", self.stem));
        let wide = string_to_u16_buffer(path.to_string_lossy());

        let file = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ,
                std::ptr::null_mut(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if file == INVALID_HANDLE_VALUE {
            tracing::warn!(pid = self.pid, "crash monitor: failed to open dump file");
            return None;
        }

        let mut exception = MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: info.thread_id,
            ExceptionPointers: info.exception_pointers as *mut _,
            // The pointers live in the crashed process.
            ClientPointers: 1,
        };
        let ok = dump::write_dump(
            self.process.as_ptr() as RawHandle,
            self.pid,
            file as RawHandle,
            Some(&mut exception),
            config.full_memory,
        );
        unsafe { CloseHandle(file) };

        if ok {
            tracing::info!(pid = self.pid, path = %path.display(), "crash monitor: wrote minidump");
            Some(path)
        } else {
            tracing::warn!(pid = self.pid, "crash monitor: MiniDumpWriteDump failed");
            None
        }
    }
}

/// Exits the monitor when the root CLI process dies, cleaning up an ephemeral session dir.
///
/// The CLI kills the monitor on a clean drop, but it may exit via `exec` without running
/// destructors. This watchdog makes sure the monitor never outlives its session.
///
/// When the session dir is ephemeral (the CLI made it because no log path was set) and nothing
/// crashed, the dir is removed so clean sessions leave nothing behind. A crash keeps its bundle.
///
/// # Arguments
///
/// * `root_pid` - the root CLI process id to wait on. `0` disables the watchdog.
/// * `registry` - the shared session state, read to see whether anything crashed.
/// * `config` - the session-wide output policy; its `dump_directory` is removed when ephemeral.
fn watch_root(root_pid: u32, registry: Arc<Mutex<Registry>>, config: Arc<MonitorConfig>) {
    if root_pid == 0 {
        return;
    }

    let handle = unsafe { OpenProcess(SYNCHRONIZE, FALSE, root_pid) };
    if handle.is_null() {
        // The CLI no longer force-kills us, so a monitor that cannot watch its root must not
        // linger.
        std::process::exit(0);
    }

    unsafe {
        WaitForSingleObject(handle, INFINITE);
        CloseHandle(handle);
    }

    // The session ended. If a crash dialog is up, let it block until the user dismisses it before
    // tearing down — the CLI no longer kills us out from under it. A short grace lets an in-flight
    // crash's dialog appear first.
    std::thread::sleep(Duration::from_millis(750));
    while report::dialog_showing() {
        std::thread::sleep(Duration::from_millis(200));
    }

    // Keep an ephemeral session dir only when something crashed; otherwise remove it.
    let crashed = registry
        .lock()
        .map(|registry| registry.reported)
        .unwrap_or(true);
    if config.ephemeral && !crashed {
        let _ = std::fs::remove_dir_all(&config.dump_directory);
    }

    tracing::info!(
        root_pid,
        "crash monitor: root process exited, shutting down"
    );
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAIL_FAST: u32 = 0xC000_0409;
    const CTRL_C: u32 = 0xC000_013A;

    #[test]
    fn clean_shutdown_wins_over_everything() {
        // A clean signal is a normal close regardless of debugger state or a fault-looking code.
        assert_eq!(
            classify_death(true, true, FAIL_FAST),
            DeathVerdict::CleanShutdown,
        );
    }

    #[test]
    fn debugger_stop_beats_a_kill() {
        // No clean signal, but debugged: an intentional stop, not a reportable kill.
        assert_eq!(classify_death(false, true, 0), DeathVerdict::DebuggerStop);
    }

    #[test]
    fn application_level_exit_is_not_reported() {
        // Ctrl-C is the runtime's business even with no clean signal and no debugger.
        assert_eq!(
            classify_death(false, false, CTRL_C),
            DeathVerdict::ApplicationLevel,
        );
    }

    #[test]
    fn bare_death_is_reportable() {
        // No clean signal, not debugged, an ordinary exit code: an external kill.
        assert_eq!(classify_death(false, false, 0), DeathVerdict::Reportable);
        assert_eq!(classify_death(false, false, 7), DeathVerdict::Reportable);
    }
}

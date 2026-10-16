//! The server side of the monitor — the accept-and-watch loop the CLI runs.
//!
//! [`serve`] binds nothing itself; it takes a listener and blocks, accepting registrations. Each
//! registration becomes a [`PidWatch`] running on its own thread, which waits on the process and
//! its crash event: a crash signal triggers an out-of-process dump and a report, while a death with
//! no signal is classified (clean shutdown, debugger stop, application-level exit, or external
//! kill). A registration that carries a startup problem is recorded before the ack and reported at
//! once instead, and nothing watches it.

use std::{
    collections::{HashMap, hash_map::Entry},
    fs::{File, OpenOptions},
    io::{self, Write},
    net::{TcpListener, TcpStream},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, RawHandle},
    },
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError},
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
        minwinbase::STILL_ACTIVE,
        processthreadsapi::{GetExitCodeProcess, OpenProcess},
        synchapi::{CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject},
        winbase::{INFINITE, WAIT_OBJECT_0},
        winnt::{
            FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GENERIC_WRITE, PAGE_READWRITE,
            PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, SYNCHRONIZE,
        },
    },
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
        },
    },
    Win32::{
        Foundation::UNICODE_STRING,
        Security::{SECURITY_QUALITY_OF_SERVICE, SECURITY_STATIC_TRACKING, SecurityIdentification},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
            FILE_SHARE_WRITE, FILE_TRAVERSE, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
        },
        System::{
            Diagnostics::Debug::MINIDUMP_EXCEPTION_INFORMATION,
            IO::IO_STATUS_BLOCK,
            Kernel::{OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE},
        },
    },
};

use super::{
    ACK_FAILED, ACK_READY, CLEAN_EVENT_PREFIX, CRASH_EVENT_PREFIX, CrashInfo, DONE_EVENT_PREFIX,
    INFO_SECTION_PREFIX, InitReport, KIND_INIT_FAILURE, MAX_STEM_LEN, Registration, STEM_PREFIX,
    is_log_name, read_reason, read_registration,
};
use crate::diagnostics::{
    dump,
    handle::OwnedHandle,
    report::{self, CrashReport, Incident, Outcome, ProcessNode, SessionLog},
};

/// The monitor's output policy: where artifacts go and what dumps to capture.
///
/// Set once by the CLI and held by the monitor's `Session`, which every thread shares, so no
/// function carries the same handful of session-wide settings as arguments.
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
    /// Whether any crash was surfaced this session — the "session crashed" flag. It finalizes the
    /// session archive at teardown and keeps an ephemeral session dir from being cleaned up.
    reported: bool,
    /// Every reportable incident this session, in order. The single session archive is rebuilt
    /// from this list as each one lands, so nothing a busy session produces is lost.
    incidents: Vec<Incident>,
    /// The session stamp the session archive file name is built from.
    session_id: String,
    /// The startup incident of each pid that has one.
    ///
    /// A failed startup can be reported twice: by the parent, which waited for readiness in vain,
    /// and by the layer itself once it gives up. The first report owns the incident; a later one
    /// adds its reason to it rather than filing a second report.
    init_incidents: HashMap<u32, InitIncident>,
    /// Reports accepted but not yet written, or whose dialog is still open. See [`PendingReport`].
    pending_reports: usize,
    /// Whether the session is being torn down, after which it accepts no more reports.
    closed: bool,
}

impl Registry {
    fn new(root_pid: u32) -> Self {
        Self {
            root_pid,
            nodes: Vec::new(),
            popup_shown: false,
            reported: false,
            incidents: Vec::new(),
            session_id: format!("{}_{}", crate::diagnostics::timestamp(), root_pid),
            init_incidents: HashMap::new(),
            pending_reports: 0,
            closed: false,
        }
    }
}

/// The state every thread of the monitor shares.
struct Session {
    /// The session-wide output policy.
    config: MonitorConfig,
    /// Everything recorded about the session's processes and reports. Taken only through
    /// [`Session::lock`].
    registry: Mutex<Registry>,
    /// Notified whenever a pending report ends, for the teardown, which waits until none is left.
    idle: Condvar,
    /// Taken by every report while it writes its files, never across a dialog, so that when two
    /// reports land for the same pid, its files end up with both reasons whichever writes last.
    report_files: Mutex<()>,
    /// Where the logs of registered processes have to be.
    log_directory: LogDirectory,
}

impl Session {
    fn new(root_pid: u32, config: MonitorConfig) -> Self {
        Self {
            log_directory: LogDirectory::new(config.dump_directory.clone()),
            config,
            registry: Mutex::new(Registry::new(root_pid)),
            idle: Condvar::new(),
            report_files: Mutex::new(()),
        }
    }

    /// Takes [`Session::report_files`]. A poisoned lock is taken all the same: every report
    /// writes its files whole.
    fn report_files(&self) -> MutexGuard<'_, ()> {
        self.report_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks the registry. Every access goes through here, with one rule for a poisoned lock: it
    /// is taken all the same. Giving up instead would leave the session counting reports it never
    /// writes, while whatever a panicking thread left half-recorded costs at most its own report.
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A pid's startup incident. See [`Registry::init_incidents`].
struct InitIncident {
    /// The stem of the incident's files, from whichever report came first.
    stem: String,
    /// Every reason reported so far, first one first.
    reason: String,
    /// Whether any report called it a failure. A slow start alone is a note.
    failed: bool,
}

/// Counts one report in [`Registry::pending_reports`] for as long as it lives.
///
/// A report is counted before anything slow happens for it (a module list, a dump) and before the
/// process it is about is acknowledged, and it is recorded in the registry before that
/// acknowledgement too. The watchdog tears the session down only once no report is pending, and
/// closes the session in the same lock, so a report is either counted before the teardown starts,
/// and waited for, or refused outright.
struct PendingReport(Arc<Session>);

impl PendingReport {
    /// Counts a report the session is about to handle.
    ///
    /// # Returns
    ///
    /// `None` once the session is being torn down: the report would land in files that are being
    /// finalized or removed.
    fn start(session: &Arc<Session>) -> Option<Self> {
        let mut registry = session.lock();
        if registry.closed {
            return None;
        }
        registry.pending_reports += 1;
        Some(Self(Arc::clone(session)))
    }
}

impl Drop for PendingReport {
    fn drop(&mut self) {
        self.0.lock().pending_reports -= 1;
        self.0.idle.notify_all();
    }
}

/// Whether a name a client gives is a plain file name of the shape mirrord builds, safe to join
/// onto the session directory or to open relative to it.
///
/// The monitor accepts registrations from any local process, and both the incident stem and the
/// layer log's name name files in the session directory. So each has to start with its fixed
/// prefix, hold only ASCII letters, digits, `-` and `_`, and be no longer than any name mirrord
/// builds. That leaves no room for a path separator, `..`, a drive or root, an alternate data
/// stream, a trailing dot or space, an 8.3 short name, or a reserved device name.
///
/// # Arguments
///
/// * `name` - what the client sent.
/// * `prefix` - the prefix every such name starts with.
/// * `max_len` - the longest such name mirrord builds.
pub(super) fn is_plain_file_name(name: &str, prefix: &str, max_len: usize) -> bool {
    name.len() <= max_len
        && name
            .strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The session's log directory, the only place the monitor opens a registered log in.
///
/// The session archive bundles every registered log, so a file outside that directory would let
/// any local process have an arbitrary file copied into it.
struct LogDirectory {
    /// As configured: the value the layers join their log names onto. The CLI makes it absolute
    /// before it hands it to the monitor and the layers, since a relative path would mean a
    /// different directory in each process's working directory.
    path: PathBuf,
    /// Held open for the whole session. Logs are opened relative to this handle, so no link on the
    /// way to the directory can send them elsewhere. Opened by the first registration that needs
    /// it, since the layers may create the directory after the monitor starts.
    opened: OnceLock<File>,
}

impl LogDirectory {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            opened: OnceLock::new(),
        }
    }

    /// The directory, opened, unless it is itself a link: whatever it leads to is not the
    /// directory the session made, and its contents are not the session's to bundle.
    fn opened(&self) -> Option<&File> {
        if let Some(opened) = self.opened.get() {
            return Some(opened);
        }

        let handle = OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(&self.path)
            .ok()?;
        let metadata = handle.metadata().ok()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }

        Some(self.opened.get_or_init(|| handle))
    }

    /// A client's log, opened, when its name is a layer log's (see [`is_log_name`]) and
    /// names an existing file in the directory.
    ///
    /// The file is opened by that name relative to the open directory, refusing every link (see
    /// [`open_without_links`]). The archive later reads that same open file (see [`SessionLog`]),
    /// so nothing swapped in under the name afterwards is read instead.
    fn open(&self, log_name: &str) -> Option<SessionLog> {
        if !is_log_name(log_name) {
            return None;
        }

        let file = open_without_links(self.opened()?, log_name)?;
        file.metadata().ok()?.is_file().then(|| SessionLog {
            name: log_name.to_owned(),
            file: Arc::new(file),
        })
    }
}

/// Opens the file `name` inside `directory`, for reading.
///
/// `OBJ_DONT_REPARSE` makes the open fail at a reparse point instead of following it. A symbolic
/// link in the session directory can lead to another machine, and following it would already sign
/// in there as the monitor's user, before any check on the opened file could refuse it. The open
/// also asks for identification-level impersonation only, so a pipe server cannot act as the
/// monitor's user.
fn open_without_links(directory: &File, name: &str) -> Option<File> {
    let mut wide = name.encode_utf16().collect::<Vec<u16>>();
    let length = u16::try_from(wide.len() * std::mem::size_of::<u16>()).ok()?;
    let object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: wide.as_mut_ptr(),
    };
    let quality_of_service = SECURITY_QUALITY_OF_SERVICE {
        Length: std::mem::size_of::<SECURITY_QUALITY_OF_SERVICE>() as u32,
        ImpersonationLevel: SecurityIdentification,
        ContextTrackingMode: SECURITY_STATIC_TRACKING,
        EffectiveOnly: 0,
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: directory.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: (OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE) as u32,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: (&raw const quality_of_service).cast(),
    };

    let mut handle = std::ptr::null_mut();
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_GENERIC_READ,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    (status >= 0).then(|| unsafe { File::from_raw_handle(handle) })
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

    let session = Arc::new(Session::new(root_pid, config));

    // Exit when the root CLI dies, so the monitor never outlives its session even if the CLI exits
    // via `exec` without running destructors. The watchdog also cleans up an ephemeral session dir.
    {
        let session = Arc::clone(&session);
        std::thread::spawn(move || watch_root(root_pid, session));
    }

    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                if let Err(error) = accept(&mut stream, &session) {
                    // A short-lived process can die between its connect and its handshake, which
                    // is not a fault. A timeout is a live client that stalled, so it still warns.
                    let client_went_away = matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::UnexpectedEof
                    );
                    if client_went_away {
                        tracing::debug!(
                            %error,
                            "crash monitor: a process went away before it finished registering"
                        );
                    } else {
                        tracing::warn!(%error, "crash monitor: failed to accept a registration");
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "crash monitor: accept error"),
        }
    }

    Ok(())
}

/// Handles one registration: record it, prepare the objects, ack, and spawn the watcher.
///
/// A registration that carries a startup problem is recorded and acked, and its files are written
/// and its dialog shown on a thread of its own: the dialog blocks until dismissed, and this loop
/// must keep accepting in the meantime. Such a report is made on a process's behalf and carries no
/// log (see [`super::report_init_failure_for`]).
///
/// A process's log is opened after the ack, on a thread of its own, so however long the open
/// takes, it holds up neither the process, nor its watch, nor the next registration.
fn accept(stream: &mut TcpStream, session: &Arc<Session>) -> io::Result<()> {
    // A client that connects but stalls before writing must not wedge the whole accept loop.
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;

    let mut registration = read_registration(stream)?;
    tracing::info!(
        pid = registration.pid,
        parent_pid = registration.parent_pid,
        name = %registration.name,
        role = %registration.role,
        "crash monitor: registration",
    );

    if !is_plain_file_name(&registration.stem, STEM_PREFIX, MAX_STEM_LEN) {
        tracing::warn!(
            pid = registration.pid,
            "crash monitor: refused a registration whose stem is not a plain file name"
        );
        stream.write_all(&[ACK_FAILED])?;
        stream.flush()?;
        return Ok(());
    }
    // A report on a process's behalf names a process that may have registered itself already;
    // the tree shows it once.
    {
        let mut registry = session.lock();
        if !(registration.init_report.is_some()
            && registry
                .nodes
                .iter()
                .any(|node| node.pid == registration.pid))
        {
            registry.nodes.push(ProcessNode {
                pid: registration.pid,
                parent_pid: registration.parent_pid,
                name: registration.name.clone(),
                role: registration.role.clone(),
                log: None,
                exit_code: None,
            });
        }
    }

    if let Some(report) = registration.init_report.take() {
        // Counted and recorded before the ack. A launcher may exit the moment it reads the ack,
        // and when it is the session's root that starts the teardown, which must then find this
        // incident and wait for its files.
        let Some(pending) = PendingReport::start(session) else {
            tracing::debug!(
                pid = registration.pid,
                "crash monitor: refused a startup report, the session is over"
            );
            stream.write_all(&[ACK_FAILED])?;
            stream.flush()?;
            return Ok(());
        };
        let claim = record_init_incident(
            session,
            &IncidentSubject {
                pid: registration.pid,
                name: &registration.name,
                stem: &registration.stem,
            },
            &report,
        );
        let acked = stream.write_all(&[ACK_READY]).and_then(|()| stream.flush());

        // Written whether or not the ack arrived: the reason is already here, so the report does
        // not depend on anything the process does next, or on it still being alive.
        let session = Arc::clone(session);
        std::thread::spawn(move || {
            let _pending = pending;
            report_init_failure(&registration, &claim, &session);
        });
        return acked;
    }

    let watch = prepare(&registration);
    let ack = if watch.is_some() {
        ACK_READY
    } else {
        ACK_FAILED
    };

    // The watcher starts only once the ack is out. A client that never got it fell back to its
    // in-process dump and holds no channel, so it never sets the clean-shutdown event, and a
    // watcher would report its ordinary exit as a kill. A process that dies between the ack and
    // the watcher is still seen: the process handle was opened in `prepare`, before the ack, and
    // stays signaled. A successful write does not prove the client read the ack; a client that
    // timed out just after it is the one case this cannot tell apart.
    stream.write_all(&[ack])?;
    stream.flush()?;

    if let Some(watch) = watch {
        let session = Arc::clone(session);
        std::thread::spawn(move || watch.run(session));
    }
    if let Some(log_name) = registration.log_name.take() {
        let pid = registration.pid;
        let session = Arc::clone(session);
        std::thread::spawn(move || attach_log(&session, pid, &log_name));
    }

    Ok(())
}

/// Opens a registered process's log and records it on the process's node, for every report
/// composed after it.
///
/// Nothing waits for it. Once the teardown has closed the session, no log is opened or recorded
/// any more, so the files being finalized or removed never gain one, and an open still stuck by
/// then ends with the monitor.
fn attach_log(session: &Session, pid: u32, log_name: &str) {
    if session.lock().closed {
        return;
    }
    let Some(log) = session.log_directory.open(log_name) else {
        tracing::warn!(
            pid,
            ?log_name,
            "crash monitor: ignored a log that is not a layer log file in the session's log \
             directory"
        );
        return;
    };

    let mut registry = session.lock();
    if registry.closed {
        return;
    }
    // The latest node of the pid is this registration's: a pid can be reused within a session.
    if let Some(node) = registry.nodes.iter_mut().rev().find(|node| node.pid == pid) {
        node.log = Some(log);
    }
}

/// Writes the report of a startup problem someone else observed, on the process's behalf.
///
/// See [`super::report_init_failure_for`]. The incident is already recorded; the process may
/// already be gone, and everything it can still offer (its exit code, its module list) is
/// best-effort.
///
/// # Arguments
///
/// * `registration` - the process, as the reporter described it.
/// * `claim` - what [`record_init_incident`] recorded for it.
/// * `session` - the shared session state.
fn report_init_failure(registration: &Registration, claim: &InitClaim, session: &Session) {
    let process = OwnedHandle::kernel(unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | SYNCHRONIZE,
            FALSE,
            registration.pid,
        )
    });

    let modules = process.as_ref().and_then(|process| {
        if let Some(exit_code) = exit_code_of(process).filter(|code| *code != STILL_ACTIVE) {
            mark_dead(session, registration.pid, exit_code);
        }
        let modules = crate::modules::module_inventory(process.as_ptr() as HANDLE);
        (!modules.is_empty()).then_some(modules)
    });

    write_init_incident(
        session,
        registration.pid,
        &registration.name,
        claim,
        modules.as_deref(),
    );
}

/// What [`record_init_incident`] recorded, for the thread that writes the files.
struct InitClaim {
    /// The stem of the incident's files.
    stem: String,
    /// Whether this report claimed the session's single dialog.
    show_dialog: bool,
}

/// Records a startup report in the registry, once per pid.
///
/// The first report for a pid files the incident. A later one, from the other side of the same
/// startup, adds its reason to that incident and files nothing new, so the session holds one
/// startup incident per process, however many sides saw it. A failure marks the session as
/// crashed and may claim the dialog; a slow start alone does neither.
///
/// # Returns
///
/// The incident's stem and whether this report claimed the dialog.
fn record_init_incident(
    session: &Session,
    subject: &IncidentSubject<'_>,
    report: &InitReport,
) -> InitClaim {
    let mut registry = session.lock();
    let failed = matches!(report, InitReport::Failed(_));

    let (stem, new_incident, newly_failed) = match registry.init_incidents.entry(subject.pid) {
        Entry::Vacant(entry) => {
            entry.insert(InitIncident {
                stem: subject.stem.to_owned(),
                reason: report.reason().to_owned(),
                failed,
            });
            (subject.stem.to_owned(), true, failed)
        }
        Entry::Occupied(mut entry) => {
            let incident = entry.get_mut();
            incident.reason = format!(
                "{}\n\nReported again for the same process: {}",
                incident.reason,
                report.reason()
            );
            let newly_failed = failed && !incident.failed;
            incident.failed |= failed;
            (incident.stem.clone(), false, newly_failed)
        }
    };

    if new_incident {
        registry.incidents.push(Incident {
            name: subject.name.to_owned(),
            pid: subject.pid,
            stem: stem.clone(),
        });
    }
    let show_dialog = newly_failed && !registry.popup_shown;
    registry.popup_shown |= show_dialog;
    registry.reported |= newly_failed;

    InitClaim { stem, show_dialog }
}

/// Writes the files of a recorded startup incident, with every reason it has so far, then shows
/// its dialog when it claimed one.
///
/// # Arguments
///
/// * `session` - the shared session state.
/// * `pid` - the process the incident is about.
/// * `name` - its image name.
/// * `claim` - what [`record_init_incident`] recorded.
/// * `modules` - the process's module inventory, when enumerated.
fn write_init_incident(
    session: &Session,
    pid: u32,
    name: &str,
    claim: &InitClaim,
    modules: Option<&str>,
) {
    let files = session.report_files();

    // Read under the files lock, so the last writer of a pid's files has every reason.
    let (snapshot, incident) = Snapshot::take(session, |registry| {
        registry
            .init_incidents
            .get(&pid)
            .map(|incident| (incident.reason.clone(), incident.failed))
    });
    let Some((reason, failed)) = incident else {
        return;
    };
    let outcome = if failed {
        Outcome::InitFailed { reason }
    } else {
        Outcome::SlowStart { reason }
    };

    write_and_show(
        session,
        &snapshot,
        &IncidentSubject {
            pid,
            name,
            stem: &claim.stem,
        },
        Findings {
            outcome,
            dump: None,
            modules,
        },
        claim.show_dialog,
        files,
    );
}

/// A registered pid the monitor watches. Holds the per-pid objects.
///
/// Each handle and the mapped view are an [`OwnedHandle`], so the watch frees everything by
/// dropping — there is no teardown method and the watcher thread can take the value by value.
struct PidWatch {
    pid: u32,
    parent_pid: u32,
    name: String,
    /// The shared incident stem, used to name the report/record/modules/dump files and to find the
    /// in-proc record.
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
    /// No clean signal, no debugger, and the exit code could not be read.
    ///
    /// Kept apart from [`DeathVerdict::TerminatedButSucceeded`]: an unreadable code is not a
    /// success, and it is not evidence of a fault either.
    ExitCodeUnavailable,
    /// The layer signalled a clean shutdown — a normal close at any exit code.
    CleanShutdown,
    /// The process was being debugged — an intentional developer stop, not a fault.
    DebuggerStop,
    /// The exit code is a runtime-defined exception or a Ctrl-C the runtime reports itself.
    ApplicationLevel,
    /// Ended from outside, but with a success code, so nothing went wrong.
    ///
    /// An external kill runs no `DLL_PROCESS_DETACH`, so the layer never sets the clean-shutdown
    /// event however well the process did. Build tools end helper processes this way as a matter
    /// of course: MSBuild stops its Roslyn compiler server and its worker nodes when it is done,
    /// and each of them exits `0`. Those are not faults, and a crash bundle for each one buries
    /// the real failure in a build.
    TerminatedButSucceeded,
    /// No clean signal, no debugger, no application-level code, and a failure exit code.
    Reportable {
        /// The failure exit code.
        exit_code: u32,
    },
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
/// * `exit_code` - the process exit code, `None` when it could not be read.
///
/// # Returns
///
/// The [`DeathVerdict`] for the death.
fn classify_death(clean: bool, debugged: bool, exit_code: Option<u32>) -> DeathVerdict {
    if clean {
        return DeathVerdict::CleanShutdown;
    }
    if debugged {
        return DeathVerdict::DebuggerStop;
    }
    let Some(exit_code) = exit_code else {
        return DeathVerdict::ExitCodeUnavailable;
    };
    if report::is_app_level_exit(exit_code) {
        DeathVerdict::ApplicationLevel
    } else if exit_code == 0 {
        DeathVerdict::TerminatedButSucceeded
    } else {
        DeathVerdict::Reportable { exit_code }
    }
}

/// Reads a process's exit code, `None` when the query fails.
fn exit_code_of(process: &OwnedHandle) -> Option<u32> {
    let mut code: DWORD = 0;
    (unsafe { GetExitCodeProcess(process.as_ptr() as HANDLE, &mut code) } != 0).then_some(code)
}

/// The process a report is about.
struct IncidentSubject<'a> {
    pid: u32,
    name: &'a str,
    /// The shared incident stem, which names every artifact of this incident.
    stem: &'a str,
}

/// The registry state one report is composed from, copied out so the registry is not locked while
/// the files are written or the dialog is up. Only [`Session::report_files`] is held while the
/// files are written, and nothing while the dialog is up.
struct Snapshot {
    nodes: Vec<ProcessNode>,
    root_pid: u32,
    incidents: Vec<Incident>,
    session_id: String,
}

impl Snapshot {
    /// Copies the registry, along with whatever else `extra` reads under the same lock.
    fn take<T>(session: &Session, extra: impl FnOnce(&mut Registry) -> T) -> (Self, T) {
        let mut registry = session.lock();
        let extra = extra(&mut registry);
        let snapshot = Self {
            nodes: registry.nodes.clone(),
            root_pid: registry.root_pid,
            incidents: registry.incidents.clone(),
            session_id: registry.session_id.clone(),
        };
        (snapshot, extra)
    }
}

/// Records a crash or a death as a new incident.
///
/// The first report of the session claims the single dialog; the rest are file-only. Every report
/// is written to disk regardless, and the session archive is rebuilt to include this incident, so
/// nothing is lost either way.
///
/// # Returns
///
/// The registry state to write the report from, and whether this report claimed the dialog.
fn record_incident(session: &Session, subject: &IncidentSubject<'_>) -> (Snapshot, bool) {
    Snapshot::take(session, |registry| {
        let show_dialog = !registry.popup_shown;
        registry.popup_shown = true;
        registry.reported = true;
        registry.incidents.push(Incident {
            name: subject.name.to_owned(),
            pid: subject.pid,
            stem: subject.stem.to_owned(),
        });
        show_dialog
    })
}

/// What one report says about its process.
struct Findings<'a> {
    /// How the process ended.
    outcome: Outcome,
    /// The minidump path, when one was produced.
    dump: Option<&'a Path>,
    /// The process's module inventory, when enumerated.
    modules: Option<&'a str>,
}

/// Composes one report from a snapshot, writes its files, releases `files` (the caller's
/// [`Session::report_files`]), then shows the dialog when this report claimed it.
///
/// The dialog blocks until dismissed, so it must never be shown while a lock other reports need is
/// held: `files` is let go as soon as the files are written.
fn write_and_show(
    session: &Session,
    snapshot: &Snapshot,
    subject: &IncidentSubject<'_>,
    findings: Findings<'_>,
    show_dialog: bool,
    files: MutexGuard<'_, ()>,
) {
    let config = &session.config;
    let Findings {
        outcome,
        dump,
        modules,
    } = findings;
    // The in-proc record shares the incident stem, so the path is known without a directory
    // scan.
    let record = {
        let path = config
            .dump_directory
            .join(format!("{}.record.txt", subject.stem));
        path.exists().then_some(path)
    };

    // Bundle the exact registered log files, so a prior session's reused pid can never leak in.
    let logs = session_logs(&snapshot.nodes);

    let report = CrashReport {
        focus_pid: subject.pid,
        focus_name: subject.name,
        outcome,
        nodes: &snapshot.nodes,
        root_pid: snapshot.root_pid,
        dump,
        record: record.as_deref(),
        logs: &logs,
        modules,
        mirrord_version: &config.version,
        stem: subject.stem,
    };
    let written = report::write_artifacts(
        &report,
        &config.dump_directory,
        &snapshot.incidents,
        &snapshot.session_id,
    );
    drop(files);

    if show_dialog {
        report::show_dialog(&report, &written, &config.dump_directory);
    }
}

/// Records a registered process's death in the registry, so it stays in the process tree marked
/// dead. The node is never removed — the session keeps every process it saw.
fn mark_dead(session: &Session, pid: u32, exit_code: u32) {
    if let Some(node) = session.lock().nodes.iter_mut().find(|node| node.pid == pid) {
        node.exit_code = Some(exit_code);
    }
}

impl PidWatch {
    /// Waits on the process and its crash event until the process exits.
    ///
    /// A crash event triggers an out-of-process dump and a report. The process handle signalling
    /// means it died; if it did so with no crash signal, that death is reported as a termination.
    fn run(self, session: Arc<Session>) {
        let process = self.process.as_ptr() as HANDLE;
        let crash_event = self.crash_event.as_ptr() as HANDLE;
        let mut reported = false;

        loop {
            // The crash event comes first: `WaitForMultipleObjects` answers with the lowest
            // signaled index, and a process that signaled and then died has something to say
            // that its death alone does not.
            let handles = [crash_event, process];
            let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, INFINITE) };

            if result == WAIT_OBJECT_0 + 1 {
                let exit_code = exit_code_of(&self.process);
                let exit = exit_code.map_or_else(
                    || "unavailable".to_owned(),
                    |exit_code| format!("{exit_code:#010x}"),
                );
                // Record the death so this process stays in the tree marked dead — whatever the
                // verdict — for any report composed after it (e.g. a child whose parent died).
                if let Some(exit_code) = exit_code {
                    mark_dead(&session, self.pid, exit_code);
                }
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
                    DeathVerdict::TerminatedButSucceeded => {
                        // Killed by whoever started it, with nothing to report. Kept at debug so
                        // the process is still accounted for when reading a session's log.
                        tracing::debug!(
                            pid = self.pid,
                            name = %self.name,
                            exit,
                            "crash monitor: layer process was ended from outside with a success code, so no report is written",
                        );
                    }
                    DeathVerdict::ExitCodeUnavailable => {
                        tracing::warn!(
                            pid = self.pid,
                            parent_pid = self.parent_pid,
                            name = %self.name,
                            "crash monitor: layer process died without a clean shutdown, and its exit code could not be read, so no report is written",
                        );
                    }
                    DeathVerdict::Reportable { exit_code } if !reported => {
                        // No clean-shutdown signal and no crash signal: killed or died abruptly.
                        tracing::warn!(
                            pid = self.pid,
                            parent_pid = self.parent_pid,
                            name = %self.name,
                            exit,
                            "crash monitor: layer process died without a clean shutdown",
                        );
                        let Some(_pending) = PendingReport::start(&session) else {
                            break;
                        };
                        let subject = IncidentSubject {
                            pid: self.pid,
                            name: &self.name,
                            stem: &self.stem,
                        };
                        let (snapshot, show_dialog) = record_incident(&session, &subject);
                        write_and_show(
                            &session,
                            &snapshot,
                            &subject,
                            Findings {
                                outcome: Outcome::Terminated { exit_code },
                                dump: None,
                                modules: None,
                            },
                            show_dialog,
                            session.report_files(),
                        );
                    }
                    DeathVerdict::Reportable { .. } => {}
                }
                break;
            } else if result == WAIT_OBJECT_0 {
                // The layer signalled through the shared section, and is frozen until the done
                // event. The report is counted before the slow part (the module list, the dump)
                // and recorded before that event, so a session that ends in the meantime waits
                // for it rather than removing its directory. A session already being torn down
                // takes nothing more: the process is let go at once.
                let Some(_pending) = PendingReport::start(&session) else {
                    tracing::debug!(
                        pid = self.pid,
                        "crash monitor: ignored a signal, the session is over"
                    );
                    unsafe { SetEvent(self.done_event.as_ptr() as HANDLE) };
                    continue;
                };
                let info = unsafe { *(self.view.as_ptr() as *const CrashInfo) };
                let modules = crate::modules::module_inventory(process);
                let modules = (!modules.is_empty()).then_some(modules);
                let subject = IncidentSubject {
                    pid: self.pid,
                    name: &self.name,
                    stem: &self.stem,
                };

                if info.kind == KIND_INIT_FAILURE {
                    let reason = read_reason(&info);
                    let claim = (!reported).then(|| {
                        record_init_incident(&session, &subject, &InitReport::Failed(reason))
                    });
                    unsafe { SetEvent(self.done_event.as_ptr() as HANDLE) };
                    if let Some(claim) = claim {
                        write_init_incident(
                            &session,
                            self.pid,
                            &self.name,
                            &claim,
                            modules.as_deref(),
                        );
                    }
                } else {
                    // The minidump is always captured; full-memory is the only gated part of it.
                    let dump = self.dump(&session.config);
                    let claim = (!reported).then(|| record_incident(&session, &subject));
                    unsafe { SetEvent(self.done_event.as_ptr() as HANDLE) };
                    if let Some((snapshot, show_dialog)) = claim {
                        write_and_show(
                            &session,
                            &snapshot,
                            &subject,
                            Findings {
                                outcome: Outcome::Crashed,
                                dump: dump.as_deref(),
                                modules: modules.as_deref(),
                            },
                            show_dialog,
                            session.report_files(),
                        );
                    }
                }
                reported = true;
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

    /// Reports whether the layer signalled a clean shutdown before dying.
    ///
    /// This is the verdict the death classification rests on, not the exit code.
    fn shut_down_cleanly(&self) -> bool {
        unsafe { WaitForSingleObject(self.clean_event.as_ptr() as HANDLE, 0) == WAIT_OBJECT_0 }
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

/// The registered layer log files this session, for the session archive bundle.
fn session_logs(nodes: &[ProcessNode]) -> Vec<SessionLog> {
    nodes.iter().filter_map(|node| node.log.clone()).collect()
}

/// Exits the monitor when the root CLI process dies, cleaning up an ephemeral session dir.
///
/// The CLI never kills the monitor, so that a crash dialog can stay up after the session ends
/// until the user dismisses it. This watchdog is what ends the monitor: once the root is gone and
/// every pending report is written and dismissed, it finalizes the session's files and exits.
///
/// When the session dir is ephemeral (the CLI made it because no log path was set) and nothing
/// crashed, the dir is removed so clean sessions leave nothing behind. A crash keeps its bundle.
///
/// # Arguments
///
/// * `root_pid` - the root CLI process id to wait on. `0` disables the watchdog.
/// * `session` - the shared session state, read to see whether anything crashed. Its
///   `dump_directory` is removed when ephemeral.
fn watch_root(root_pid: u32, session: Arc<Session>) {
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

    // The session ended. A short grace lets the watchers notice processes that died with it. Then
    // every report already accepted is written before anything is torn down. A report stays
    // pending while its dialog is up, so a dialog on screen holds the teardown until the user
    // dismisses it; the CLI does not kill the monitor. Closing the session in the lock that saw no
    // report pending makes that final: a report that arrives later is refused rather than written
    // into a directory being removed.
    std::thread::sleep(Duration::from_millis(750));
    let (crashed, incidents, logs, session_id) = {
        // Every wake, a poisoned one included, checks the count again before the session closes.
        let mut registry = session.lock();
        while registry.pending_reports > 0 {
            registry = session
                .idle
                .wait(registry)
                .unwrap_or_else(PoisonError::into_inner);
        }
        registry.closed = true;
        (
            registry.reported,
            registry.incidents.clone(),
            session_logs(&registry.nodes),
            registry.session_id.clone(),
        )
    };

    let config = &session.config;
    // Finalize the session archive when something crashed — a last rebuild, then the loose dumps
    // are removed so a dump lives only inside the archive; otherwise remove an ephemeral
    // session dir.
    if crashed {
        report::finalize_session_archive(&report::SessionArchive {
            directory: &config.dump_directory,
            incidents: &incidents,
            logs: &logs,
            session_id: &session_id,
        });
    } else if config.ephemeral {
        // The registry holds every registered log open. Let go of them first, so nothing of the
        // monitor's own is in use inside the directory.
        session.lock().nodes.clear();
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
    use std::{net::TcpListener, time::Instant};

    use winapi::{
        shared::winerror::ERROR_IO_PENDING,
        um::{
            errhandlingapi::GetLastError,
            ioapiset::{CancelIoEx, DeviceIoControl, GetOverlappedResult},
            minwinbase::OVERLAPPED,
            winbase::FILE_FLAG_OVERLAPPED,
            winioctl::FSCTL_REQUEST_BATCH_OPLOCK,
        },
    };

    use super::{
        super::{LOG_PREFIX, MAX_FILE_NAME_LEN},
        *,
    };

    const FAIL_FAST: u32 = 0xC000_0409;
    const CTRL_C: u32 = 0xC000_013A;

    #[test]
    fn clean_shutdown_wins_over_everything() {
        // A clean signal is a normal close regardless of debugger state or a fault-looking code.
        assert_eq!(
            classify_death(true, true, Some(FAIL_FAST)),
            DeathVerdict::CleanShutdown,
        );
    }

    #[test]
    fn debugger_stop_beats_a_kill() {
        // No clean signal, but debugged: an intentional stop, not a reportable kill.
        assert_eq!(
            classify_death(false, true, Some(0)),
            DeathVerdict::DebuggerStop
        );
    }

    #[test]
    fn application_level_exit_is_not_reported() {
        // Ctrl-C is the runtime's business even with no clean signal and no debugger.
        assert_eq!(
            classify_death(false, false, Some(CTRL_C)),
            DeathVerdict::ApplicationLevel,
        );
    }

    #[test]
    fn bare_death_with_a_failure_code_is_reportable() {
        // No clean signal, not debugged, a failure exit code: an external kill worth a report.
        assert_eq!(
            classify_death(false, false, Some(1)),
            DeathVerdict::Reportable { exit_code: 1 }
        );
        assert_eq!(
            classify_death(false, false, Some(7)),
            DeathVerdict::Reportable { exit_code: 7 }
        );
    }

    #[test]
    fn bare_death_with_a_success_code_is_not_reported() {
        // MSBuild ends its Roslyn compiler server and its worker nodes when a build finishes.
        // Each is killed from outside, so none of them runs its detach, and each exits 0. A
        // crash bundle for every one of those buries the one process that really failed.
        assert_eq!(
            classify_death(false, false, Some(0)),
            DeathVerdict::TerminatedButSucceeded
        );
    }

    /// A scratch session directory, removed on drop.
    struct SessionDir(PathBuf);

    impl SessionDir {
        fn new(name: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "mirrord-monitor-{name}-{}-{}",
                std::process::id(),
                crate::diagnostics::timestamp()
            ));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).expect("test directory");
            Self(directory)
        }

        fn config(&self) -> MonitorConfig {
            MonitorConfig {
                version: "test".to_owned(),
                dump_directory: self.0.clone(),
                full_memory: false,
                ephemeral: true,
            }
        }
    }

    impl Drop for SessionDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A session whose dialog is already taken, so no test shows one.
    fn quiet_session(directory: &SessionDir) -> Arc<Session> {
        let session = Arc::new(Session::new(1, directory.config()));
        session.lock().popup_shown = true;
        session
    }

    /// Records and writes a startup report the way the monitor does.
    fn report_startup(session: &Session, pid: u32, stem: &str, report: InitReport) {
        let subject = IncidentSubject {
            pid,
            name: "child.exe",
            stem,
        };
        let claim = record_init_incident(session, &subject, &report);
        write_init_incident(session, pid, "child.exe", &claim, None);
    }

    /// The parent and the layer can both see the same startup fail. The session gets one
    /// incident for it, whose report carries both reasons, and a different process still gets
    /// its own.
    #[test]
    fn one_init_incident_per_process() {
        let session = SessionDir::new("one-incident");
        let monitor = quiet_session(&session);

        report_startup(
            &monitor,
            42,
            "mirrord-crash_first",
            InitReport::Failed("the layer did not report ready".to_owned()),
        );
        report_startup(
            &monitor,
            42,
            "mirrord-crash_second",
            InitReport::Failed("the proxy connection failed".to_owned()),
        );
        report_startup(
            &monitor,
            43,
            "mirrord-crash_other",
            InitReport::Failed("the layer failed in DllMain".to_owned()),
        );

        let stems = monitor
            .lock()
            .incidents
            .iter()
            .map(|incident| incident.stem.clone())
            .collect::<Vec<_>>();
        let report = std::fs::read_to_string(session.0.join("mirrord-crash_first.report.txt"))
            .expect("the first report was written");

        assert_eq!(
            stems,
            ["mirrord-crash_first", "mirrord-crash_other"],
            "one incident per process"
        );
        assert!(
            report.contains("the layer did not report ready"),
            "{report}"
        );
        assert!(report.contains("the proxy connection failed"), "{report}");
        assert!(
            !session.0.join("mirrord-crash_second.report.txt").exists(),
            "the second report joined the first"
        );
    }

    /// A slow start is a note: written, but the session is not marked as crashed and no dialog
    /// is claimed. A failure reported later for the same process still is both.
    #[test]
    fn a_slow_start_is_a_note_until_it_fails() {
        let session = SessionDir::new("slow-start");
        let monitor = Session::new(1, session.config());
        let subject = IncidentSubject {
            pid: 42,
            name: "child.exe",
            stem: "mirrord-crash_slow",
        };

        let claim = record_init_incident(
            &monitor,
            &subject,
            &InitReport::SlowStart("it did not report ready in time".to_owned()),
        );
        assert!(!claim.show_dialog, "a note never shows a dialog");
        {
            let registry = monitor.lock();
            assert!(
                !registry.reported,
                "a note does not mark the session as crashed"
            );
            assert!(!registry.popup_shown);
        }
        write_init_incident(&monitor, 42, "child.exe", &claim, None);
        let note = std::fs::read_to_string(session.0.join("mirrord-crash_slow.report.txt"))
            .expect("the note was written");
        assert!(note.contains("slow to start"), "{note}");
        assert!(!note.contains("runs without mirrord"), "{note}");

        let claim = record_init_incident(
            &monitor,
            &subject,
            &InitReport::Failed("the proxy connection failed".to_owned()),
        );
        assert!(claim.show_dialog, "the failure claims the dialog");
        assert!(monitor.lock().reported);
    }

    /// The incident exists by the time the reporter reads the ack, so a reporter that is the
    /// session's root can exit right after it without the teardown deleting the session's files,
    /// and the teardown waits for the report to be written.
    #[test]
    fn a_startup_report_is_recorded_before_the_ack() {
        let session = SessionDir::new("before-ack");
        let monitor = quiet_session(&session);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let server = {
            let monitor = Arc::clone(&monitor);
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                accept(&mut stream, &monitor).expect("registration");
            })
        };

        // A pid nothing runs as, so the writer finds no process to inspect.
        let pid = u32::MAX - 3;
        let stem = super::super::incident_stem("child", pid);
        let ack = super::super::exchange(
            address,
            &Registration {
                pid,
                parent_pid: 1,
                name: "child.exe".to_owned(),
                role: "child".to_owned(),
                stem: stem.clone(),
                log_name: None,
                init_report: Some(InitReport::Failed("the layer failed".to_owned())),
            },
            Duration::from_secs(1),
            Duration::from_secs(5),
        )
        .expect("exchange");
        assert_eq!(ack, ACK_READY);
        {
            let registry = monitor.lock();
            assert!(registry.reported, "the session is marked before the ack");
            assert!(
                registry
                    .incidents
                    .iter()
                    .any(|incident| incident.stem == stem),
                "the incident is recorded before the ack"
            );
        }

        server.join().expect("server");
        // What the teardown waits for before it removes anything.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !session.0.join(format!("{stem}.report.txt")).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the report was never written"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A layer that signals a startup failure or a crash through its channel is recorded and
    /// counted by the time the monitor lets it go, as a reporter over TCP is by the time it reads
    /// the ack. A session that ends right then waits for the report instead of removing its
    /// files.
    #[test]
    fn a_signalled_report_is_recorded_before_the_process_is_let_go() {
        let session = SessionDir::new("signalled");

        for crash in [false, true] {
            let monitor = quiet_session(&session);
            let mut child = mirrord_command::resolve_command("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("a process to watch");
            let pid = child.id();
            let stem = super::super::incident_stem("ping", pid);
            let watch = prepare(&Registration {
                pid,
                parent_pid: 1,
                name: "ping.exe".to_owned(),
                role: "child".to_owned(),
                stem: stem.clone(),
                log_name: None,
                init_report: None,
            })
            .expect("prepare the watch");
            let watcher = {
                let monitor = Arc::clone(&monitor);
                std::thread::spawn(move || watch.run(monitor))
            };
            // What the layer in that process holds.
            let channel = super::super::channel::open_channel(pid).expect("open the channel");

            let let_go = if crash {
                channel.signal_crash(0, 0)
            } else {
                channel.signal_init_failure("the layer failed")
            };

            assert!(let_go, "the monitor answered (crash: {crash})");
            {
                let registry = monitor.lock();
                assert!(registry.reported, "the session is marked (crash: {crash})");
                assert!(
                    registry
                        .incidents
                        .iter()
                        .any(|incident| incident.stem == stem),
                    "the incident is recorded (crash: {crash})"
                );
                assert!(
                    registry.pending_reports > 0
                        || session.0.join(format!("{stem}.report.txt")).exists(),
                    "the report is counted until it is written (crash: {crash})"
                );
            }

            child.kill().expect("end the process");
            child.wait().expect("reap the process");
            watcher.join().expect("the watcher ends with the process");
        }
    }

    /// A layer log's name as the layer's logger builds it: the prefix, a timestamp, the sanitized
    /// process name and the pid.
    fn layer_log_name(process_name: &str, pid: u32) -> String {
        format!(
            "{LOG_PREFIX}{}_{}_pid{pid}",
            crate::diagnostics::timestamp(),
            crate::diagnostics::sanitize_name(process_name)
        )
    }

    /// A stem is joined into file paths and a log name is opened in the session directory, so
    /// anything but a plain name of the shape mirrord builds is refused.
    #[test]
    fn only_plain_file_names_are_accepted() {
        let stem = |name: &str| is_plain_file_name(name, STEM_PREFIX, MAX_STEM_LEN);
        let log = is_log_name;

        assert!(stem(&super::super::incident_stem("python3.12", 42)));
        assert!(stem("mirrord-crash_20260101_120000_node_pid1"));
        assert!(log(&layer_log_name("python3.12", u32::MAX)));
        assert!(log("mirrord-layer_20260101_120000_my-app_v2_pid1"));
        assert!(log(&format!(
            "{LOG_PREFIX}{}",
            "a".repeat(MAX_FILE_NAME_LEN - LOG_PREFIX.len())
        )));

        for name in [
            "",
            "mirrord-crash_",
            r"..\..\pwned",
            "mirrord-crash_..",
            r"mirrord-crash_a\..\..\pwned",
            "mirrord-crash_a/../../pwned",
            r"C:\Windows\Temp\pwned",
            r"\\?\C:\pwned",
            "/pwned",
            "mirrord-crash_a:stream",
            "mirrord-crash_a.b",
            "CON",
            "nul",
            "LPT1",
            "pwned",
            &format!("mirrord-crash_{}", "a".repeat(MAX_STEM_LEN)),
        ] {
            assert!(!stem(name), "stem {name:?} must be refused");
        }

        for name in [
            "",
            "mirrord-layer_",
            r"..\..\secret.txt",
            "mirrord-layer_..",
            r"mirrord-layer_x\..\..\secret.txt",
            "mirrord-layer_x/../../secret.txt",
            r"C:\Users\me\mirrord-layer_x",
            r"C:mirrord-layer_x",
            r"\\elsewhere\share\mirrord-layer_x",
            r"\\?\C:\mirrord-layer_x",
            r"\\.\pipe\mirrord-layer_x",
            "mirrord-layer_x:stream",
            "mirrord-layer_x::$DATA",
            "mirrord-layer_x.",
            "mirrord-layer_x ",
            "MIRROR~1",
            "NUL",
            "COM1",
            "secret.txt",
            "mirrord-crash_20260101_120000_node_pid1",
            &format!("{LOG_PREFIX}{}", "a".repeat(MAX_FILE_NAME_LEN)),
        ] {
            assert!(!log(name), "log name {name:?} must be refused");
        }
    }

    /// Every stem a client builds is accepted, however long its process name, and two long names
    /// that only differ at the end still name different files.
    #[test]
    fn a_long_process_name_gives_an_accepted_stem() {
        let name = format!("{}.exe", "a-very-long-tool-name_".repeat(8));
        assert!(name.len() > 170);
        let other = format!("{}.exe", "a-very-long-tool-name_".repeat(9));

        let stem = super::super::incident_stem(&name, u32::MAX);

        assert!(
            is_plain_file_name(&stem, STEM_PREFIX, MAX_STEM_LEN),
            "{stem} ({} bytes)",
            stem.len()
        );
        assert!(stem.contains("a-very-long-tool-name_"), "{stem}");
        assert_ne!(
            super::super::stem_name(&name),
            super::super::stem_name(&other),
            "the names differ, so their stems do"
        );
    }

    /// A log is bundled into the session archive, so only a layer log that is a file in the
    /// session's log directory is opened.
    #[test]
    fn only_layer_logs_in_the_session_are_opened() {
        let session = SessionDir::new("log-names");
        let log_name = layer_log_name("child", 1);
        std::fs::write(session.0.join(&log_name), "the layer's log").expect("log");
        std::fs::write(session.0.join("secret.txt"), "secret").expect("another file");
        let subdirectory = layer_log_name("directory", 2);
        std::fs::create_dir(session.0.join(&subdirectory)).expect("a directory");

        let directory = LogDirectory::new(session.0.clone());

        let log = directory.open(&log_name).expect("a log in the directory");
        assert_eq!(log.name, log_name);
        assert_eq!(log.contents().expect("read"), b"the layer's log");
        assert!(
            directory.open("secret.txt").is_none(),
            "a file that is not a layer log"
        );
        assert!(
            directory.open(&layer_log_name("missing", 3)).is_none(),
            "a log that does not exist"
        );
        assert!(directory.open(&subdirectory).is_none(), "a directory");
        assert!(
            directory
                .open(&session.0.join(&log_name).to_string_lossy())
                .is_none(),
            "a path, even to a log in the directory"
        );
    }

    /// A link can lead anywhere, another machine included, so a log that is a link is refused
    /// without being followed, and so is a session directory that is itself a link.
    #[test]
    fn a_link_to_a_log_is_refused_without_being_followed() {
        let session = SessionDir::new("log-links");
        let outside = SessionDir::new("log-links-outside");
        let log_name = layer_log_name("child", 1);
        let secret = outside.0.join(&log_name);
        std::fs::write(&secret, "secret").expect("outside file");

        // A junction is a reparse point any user can create. It is named as a log, so only the
        // link check keeps it out.
        let junction = session.0.join(layer_log_name("junction", 2));
        let created = mirrord_command::resolve_command("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside.0)
            .output()
            .expect("run mklink");
        assert!(created.status.success(), "create the junction: {created:?}");
        let junction_name = junction
            .file_name()
            .and_then(|name| name.to_str())
            .expect("the junction's name");
        assert!(
            LogDirectory::new(session.0.clone())
                .open(junction_name)
                .is_none(),
            "a junction in the directory is refused"
        );

        let mut cases = vec![(junction.clone(), "a directory that is a junction")];
        // A file symbolic link needs developer mode or the privilege to create one.
        if std::os::windows::fs::symlink_file(&secret, session.0.join(&log_name)).is_ok() {
            cases.push((session.0.clone(), "a log that is a symbolic link"));
        }
        for (directory, what) in cases {
            // An open that reaches the file breaks the oplock and waits for it.
            let oplock = BatchOplock::request(&secret);
            let directory = LogDirectory::new(directory);
            let log_name = log_name.clone();
            let refused = std::thread::spawn(move || directory.open(&log_name).is_none());

            let followed = oplock.broken_within(Duration::from_millis(500));
            drop(oplock);
            assert!(refused.join().expect("the open ends"), "{what} is refused");
            assert!(!followed, "{what} is not followed");
        }
    }

    /// A batch oplock on a file. While it is held, another open of the file waits until the
    /// holder lets go, which this one does when dropped.
    struct BatchOplock {
        file: File,
        /// Set once another open breaks the oplock, that is, once that open is waiting.
        broken: OwnedHandle,
        /// Where the oplock request completes. Boxed, since the kernel writes it after the
        /// request returns.
        overlapped: Box<OVERLAPPED>,
    }

    impl BatchOplock {
        fn request(path: &Path) -> Self {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_OVERLAPPED)
                .open(path)
                .expect("open the file for the oplock");
            let broken = OwnedHandle::kernel(unsafe {
                CreateEventW(std::ptr::null_mut(), TRUE, FALSE, std::ptr::null())
            })
            .expect("an event");
            let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
            overlapped.hEvent = broken.as_ptr() as HANDLE;

            let requested = unsafe {
                DeviceIoControl(
                    file.as_raw_handle() as HANDLE,
                    FSCTL_REQUEST_BATCH_OPLOCK,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut *overlapped,
                )
            };
            // A granted oplock stays pending until it is broken.
            assert!(
                requested == 0 && unsafe { GetLastError() } == ERROR_IO_PENDING,
                "the oplock was not granted: {}",
                io::Error::last_os_error()
            );

            Self {
                file,
                broken,
                overlapped,
            }
        }

        /// Whether another open of the file started waiting on the oplock within `timeout`.
        fn broken_within(&self, timeout: Duration) -> bool {
            unsafe {
                WaitForSingleObject(self.broken.as_ptr() as HANDLE, timeout.as_millis() as DWORD)
                    == WAIT_OBJECT_0
            }
        }
    }

    impl Drop for BatchOplock {
        fn drop(&mut self) {
            // Let the request finish before the fields go, so the kernel never writes into a
            // freed `OVERLAPPED`. Closing the file then lets a waiting open through.
            let file = self.file.as_raw_handle() as HANDLE;
            let mut transferred: DWORD = 0;
            unsafe {
                CancelIoEx(file, &mut *self.overlapped);
                GetOverlappedResult(file, &mut *self.overlapped, &mut transferred, TRUE);
            }
        }
    }

    /// A process's log is opened after the ack and apart from its watch, so neither the process
    /// nor the watch waits on the open, however long it takes: a crash signalled meanwhile is
    /// recorded, and the log is attached once the open ends.
    #[test]
    fn a_watch_does_not_wait_for_the_log() {
        let session = SessionDir::new("log-apart");
        let monitor = quiet_session(&session);
        let log_name = "mirrord-layer_x_pid1";
        let log_path = session.0.join(log_name);
        std::fs::write(&log_path, "the layer's log").expect("log");
        let no_log = |monitor: &Session| monitor.lock().nodes.iter().all(|node| node.log.is_none());

        // Holds the monitor's open of the log until the test lets go of it.
        let oplock = BatchOplock::request(&log_path);

        let mut child = mirrord_command::resolve_command("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("a process to watch");
        let pid = child.id();
        let stem = super::super::incident_stem("ping", pid);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let server = {
            let monitor = Arc::clone(&monitor);
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                accept(&mut stream, &monitor).expect("registration");
            })
        };
        let ack = super::super::exchange(
            address,
            &Registration {
                pid,
                parent_pid: 1,
                name: "ping.exe".to_owned(),
                role: "child".to_owned(),
                stem: stem.clone(),
                log_name: Some(log_name.to_owned()),
                init_report: None,
            },
            Duration::from_secs(1),
            Duration::from_secs(5),
        )
        .expect("the registration is answered while its log cannot be opened");
        assert_eq!(ack, ACK_READY, "the process is watched");
        server.join().expect("the accept loop is free again");
        assert!(
            oplock.broken_within(Duration::from_secs(10)),
            "the monitor opens the log"
        );

        // What the layer in that process holds.
        let channel = super::super::channel::open_channel(pid).expect("open the channel");
        assert!(
            channel.signal_crash(0, 0),
            "the watch answers while the log's open waits"
        );
        {
            let registry = monitor.lock();
            assert!(
                registry
                    .incidents
                    .iter()
                    .any(|incident| incident.stem == stem),
                "the crash is recorded"
            );
        }
        assert!(no_log(&monitor), "the open is still waiting");

        drop(oplock);
        let deadline = Instant::now() + Duration::from_secs(10);
        while no_log(&monitor) || monitor.lock().pending_reports > 0 {
            assert!(
                Instant::now() < deadline,
                "the log was never attached, or the report never written"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        child.kill().expect("end the process");
        child.wait().expect("reap the process");
    }

    /// Once the teardown has closed the session, a log is no longer recorded, so the files being
    /// finalized or removed never gain one.
    #[test]
    fn no_log_is_attached_once_the_session_is_closed() {
        let session = SessionDir::new("log-closed");
        let monitor = quiet_session(&session);
        let log_name = "mirrord-layer_x_pid1";
        std::fs::write(session.0.join(log_name), "the layer's log").expect("log");
        monitor.lock().nodes.push(ProcessNode {
            pid: 7,
            parent_pid: 1,
            name: "child.exe".to_owned(),
            role: "child".to_owned(),
            log: None,
            exit_code: None,
        });
        let attached = || monitor.lock().nodes.iter().any(|node| node.log.is_some());

        monitor.lock().closed = true;
        attach_log(&monitor, 7, log_name);
        assert!(!attached(), "a closed session takes no log");

        monitor.lock().closed = false;
        attach_log(&monitor, 7, log_name);
        assert!(attached(), "an open session does");
    }

    /// The archive reads the file the monitor checked, not whatever its name leads to later, so
    /// nothing put in its place after the check is bundled instead.
    #[test]
    fn a_log_is_read_from_the_file_that_was_checked() {
        let session = SessionDir::new("log-handle");
        let log_name = layer_log_name("child", 1);
        let log_path = session.0.join(&log_name);
        std::fs::write(&log_path, "the layer's log").expect("log");

        let log = LogDirectory::new(session.0.clone())
            .open(&log_name)
            .expect("accepted");

        std::fs::rename(&log_path, session.0.join("moved")).expect("move the log away");
        std::fs::write(&log_path, "something else").expect("a file in its place");

        assert_eq!(log.contents().expect("read"), b"the layer's log");
    }

    #[test]
    fn an_unreadable_exit_code_is_not_a_success() {
        assert_eq!(
            classify_death(false, false, None),
            DeathVerdict::ExitCodeUnavailable
        );
    }
}

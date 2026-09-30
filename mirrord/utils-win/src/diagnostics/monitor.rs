//! Out-of-process crash-dump monitor.
//!
//! The monitor is a normal mirrord process, one per session, spawned by the CLI. It is the safe
//! place to dump a crashed process from the outside and to notice a process that died with no
//! handler firing at all.
//!
//! ## Two channels, on purpose
//!
//! Registration uses TCP. A layer connects once at startup, in a healthy state, and sends its pid,
//! parent pid, name, and role. The monitor opens a handle to that pid and creates the per-pid crash
//! objects, then acks. The layer opens those objects and keeps the handles.
//!
//! A registration can also carry a startup problem that someone else observed (see
//! [`report_init_failure_for`]). The monitor records that one before it acks, writes its report
//! straight away and watches nothing, because the facts are all in the message.
//!
//! Crash signalling does NOT use TCP. A crashing process cannot safely open a socket. So the crash
//! path is a per-pid named auto-reset event plus a tiny shared section. The handler writes the
//! faulting thread id and `ExceptionPointers` into the section, sets the crash event, and waits
//! briefly on a done event. The monitor wakes, dumps from the outside with `ClientPointers = TRUE`,
//! then sets the done event.
//!
//! ## Death detection
//!
//! The monitor also waits on the process handle. When the process exits it logs the exit code. This
//! is what catches an external `TerminateProcess` (the EDR-kill hypothesis), where no handler ever
//! runs and no dump is possible.
//!
//! ## Layout
//!
//! This file is the shared protocol both sides agree on: the per-pid object names, the
//! `CrashInfo` shared-section layout, the [`Registration`] wire format, and its codec. The two
//! sides live in submodules: `channel` is what a registered layer holds and signals through,
//! `server` is the monitor's accept-and-watch loop.

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

mod channel;
mod server;

pub use channel::{MonitorChannel, register};
pub use server::{MonitorConfig, serve};

/// Reports a layer-startup problem of another process, on that process's behalf.
///
/// A layer that fails inside `DllMain` cannot register here itself. Registration opens a socket,
/// which must not run under the loader lock, and a layer that failed there starts no thread of its
/// own to do it later. All it can do is set a named event. Whoever waited on that event calls this,
/// from a place with neither constraint. The same goes for a layer that never reported at all, and
/// for an injection that failed after the process was created.
///
/// The reason travels in the registration itself, so the report depends neither on the failed
/// process still being alive nor on anything it signals. The monitor takes a registration as data
/// and never checks who sent it, so the incident it composes names the process that failed, not
/// the caller.
///
/// The report carries no layer log. Only the failed process knows the file name its logger chose,
/// so the reason text has to point the reader at the log directory instead.
///
/// The caller is usually a `CreateProcess` detour on one of the target's threads, so the whole
/// exchange (connect, send, acknowledgement) is bounded by `INIT_FAILURE_REPORT_BUDGET`. The
/// monitor has recorded the incident by the time it acknowledges, so a launcher that exits right
/// after this call does not take the session's files with it.
///
/// # Arguments
///
/// * `monitor` - the monitor of the failed process's session. A launcher reads it from the
///   environment it gave the process, not from its own: `mirrord exec` keeps it only there.
/// * `pid` - the process whose layer failed.
/// * `parent_pid` - the caller, recorded as that process's parent in the report tree.
/// * `report` - what happened, and the text the report shows.
pub fn report_init_failure_for(monitor: SocketAddr, pid: u32, parent_pid: u32, report: InitReport) {
    // This is mirrord's own traffic, and it runs wherever the caller waited for the child - which
    // for the process hook is a detour on one of the target's threads, with every hook live. The
    // socket below would then be intercepted and routed to the cluster, where nothing is listening
    // on the monitor's port. The marker sends it straight to `ws2_32` instead, the same way the
    // crash handler's own registration reaches the monitor from the layer's worker thread.
    let _internal = crate::internal_thread::InternalGuard::enter();

    let name = crate::process::process_status(pid).name;
    let name = if name.is_empty() {
        format!("pid {pid}")
    } else {
        name
    };

    let registration = Registration {
        pid,
        parent_pid,
        name: name.clone(),
        role: "child (layer failed to start)".to_owned(),
        stem: incident_stem(&name, pid),
        log_name: None,
        init_report: Some(report),
    };

    match exchange(
        monitor,
        &registration,
        INIT_FAILURE_REPORT_CONNECT_TIMEOUT,
        INIT_FAILURE_REPORT_BUDGET,
    ) {
        Ok(ACK_READY) => {}
        Ok(_) => tracing::warn!(
            pid,
            %monitor,
            "the crash monitor refused the report for a layer that failed to start"
        ),
        Err(error) => tracing::warn!(
            pid,
            %monitor,
            %error,
            "the crash monitor took no report for a layer that failed to start"
        ),
    }
}

/// The stem every artifact of one incident is named from: a fixed prefix, a timestamp, the
/// sanitized process name, bounded (see [`stem_name`]), and the pid.
///
/// Both the layer and a launcher reporting on a process's behalf build stems here, and the monitor
/// accepts only stems of this shape and at most [`MAX_STEM_LEN`] long (see
/// `server::is_plain_file_name`), because it joins the stem into file paths.
pub(crate) fn incident_stem(process_name: &str, pid: u32) -> String {
    format!(
        "{STEM_PREFIX}{}_{}_pid{pid}",
        super::timestamp(),
        stem_name(process_name)
    )
}

/// The process name as [`incident_stem`] uses it: sanitized, and at most [`STEM_NAME_LEN`] long.
///
/// A longer name keeps its beginning, which is what a reader recognizes, and ends in a hash of the
/// whole name instead of the rest, so two long names that only differ towards the end still give
/// different stems.
fn stem_name(process_name: &str) -> String {
    let sanitized = super::sanitize_name(process_name);
    if sanitized.len() <= STEM_NAME_LEN {
        return sanitized;
    }

    let mut hasher = DefaultHasher::new();
    process_name.hash(&mut hasher);
    let hash = format!("-{:08x}", hasher.finish() as u32);
    // Sanitized names are ASCII, so any byte offset is a character boundary.
    let kept = sanitized
        .get(..STEM_NAME_LEN - hash.len())
        .unwrap_or_default();
    format!("{kept}{hash}")
}

/// The prefix of every incident stem. See [`incident_stem`].
const STEM_PREFIX: &str = "mirrord-crash_";

/// The longest process name an incident stem carries. See [`stem_name`].
const STEM_NAME_LEN: usize = 64;

/// The longest stem [`incident_stem`] builds: the prefix, a `%Y%m%d_%H%M%S` timestamp, the
/// bounded name and the largest pid, with their separators.
///
/// Far below a file name's limit, with room for the longest suffix an incident's files add.
const MAX_STEM_LEN: usize = STEM_PREFIX.len()
    + "20260101_120000".len()
    + "_".len()
    + STEM_NAME_LEN
    + "_pid".len()
    + "4294967295".len();

/// The prefix of every layer log file name.
///
/// This mirrors the name `mirrord_layer_lib::logging` gives its log files: the prefix, a
/// timestamp, the sanitized process name and the pid. utils-win cannot depend on layer-lib, so
/// each crate keeps its own copy, and a layer-lib test checks that the copies match and that the
/// names its logger builds pass [`is_log_name`].
pub const LOG_PREFIX: &str = "mirrord-layer_";

/// The longest file name Windows file systems take, and so the longest layer log name that can
/// exist.
const MAX_FILE_NAME_LEN: usize = 255;

/// Whether `name` is a layer log's file name the monitor opens in the session directory.
///
/// A registered log is opened only under such a name, so a layer whose log names failed this
/// would have its log missing from every crash bundle.
pub fn is_log_name(name: &str) -> bool {
    server::is_plain_file_name(name, LOG_PREFIX, MAX_FILE_NAME_LEN)
}

/// How long [`report_init_failure_for`] may spend connecting.
const INIT_FAILURE_REPORT_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the whole [`report_init_failure_for`] exchange may take, connect included.
const INIT_FAILURE_REPORT_BUDGET: Duration = Duration::from_secs(3);

/// Sends a registration and reads the monitor's one-byte answer, all within `budget`.
///
/// The registration is encoded and checked against the monitor's size limit before anything is
/// sent.
///
/// # Arguments
///
/// * `address` - the monitor's TCP endpoint.
/// * `registration` - what to send.
/// * `connect_timeout` - how much of the budget the connect may use.
/// * `budget` - the bound on the whole exchange.
///
/// # Returns
///
/// The acknowledgement byte, [`ACK_READY`] or [`ACK_FAILED`].
fn exchange(
    address: SocketAddr,
    registration: &Registration,
    connect_timeout: Duration,
    budget: Duration,
) -> io::Result<u8> {
    let deadline = Instant::now() + budget;
    let remaining = || {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the crash monitor did not answer in time",
            ))
        } else {
            Ok(left)
        }
    };

    let message = encode_registration(registration)?;

    let mut stream = TcpStream::connect_timeout(&address, connect_timeout.min(budget))?;

    // Not `write_all`: a socket timeout bounds one call, so a monitor that drains slowly would
    // restart it on every partial write and stretch the exchange past the budget. Each write gets
    // only what is left of it.
    let mut unsent = message.as_slice();
    while !unsent.is_empty() {
        stream.set_write_timeout(Some(remaining()?))?;
        match stream.write(unsent) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => unsent = unsent.get(written..).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }

    // One byte arrives in one read, so one timeout bounds it.
    stream.set_read_timeout(Some(remaining()?))?;
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack)?;
    Ok(ack[0])
}

/// Name prefix for the per-pid crash event. The handler sets it; the monitor waits on it.
const CRASH_EVENT_PREFIX: &str = "mirrord_crash_event_";
/// Name prefix for the per-pid done event. The monitor sets it; the handler waits on it.
const DONE_EVENT_PREFIX: &str = "mirrord_crash_done_";
/// Name prefix for the per-pid clean-shutdown event. The layer sets it on `DLL_PROCESS_DETACH`.
///
/// This is the signal that distinguishes a normal close from a kill or a crash. An orderly exit
/// runs the detach and sets it; an abrupt `TerminateProcess` or a crash does not. The monitor reads
/// its state when the process dies. So the death verdict never has to guess from the exit code.
const CLEAN_EVENT_PREFIX: &str = "mirrord_crash_clean_";
/// Name prefix for the per-pid shared crash-info section.
const INFO_SECTION_PREFIX: &str = "mirrord_crash_info_";

/// Registration accepted; the per-pid objects are ready to open.
pub const ACK_READY: u8 = 1;
/// Registration rejected; the monitor could not prepare.
const ACK_FAILED: u8 = 0;

/// Upper bound on a registration message, to guard the reader.
const MAX_MESSAGE_LEN: usize = 64 * 1024;

/// How long the crashing process waits for the monitor to finish the dump.
const DUMP_ACK_TIMEOUT_MS: u32 = 10_000;

/// A `CrashInfo.kind` for a real crash: a dump should be written.
const KIND_CRASH: u32 = 0;
/// A `CrashInfo.kind` for an early layer-init failure: no exception, no dump, just `reason`.
const KIND_INIT_FAILURE: u32 = 1;
/// Capacity of the init-failure reason carried in the shared section.
const REASON_CAPACITY: usize = 512;

/// The crash facts shared from the crashing process to the monitor.
///
/// This is the `#[repr(C)]` layout of the per-pid shared section (`mirrord_crash_info_<pid>`). The
/// layer maps it and writes; the monitor maps the same named section and reads. Both sides must
/// agree on this layout for the shared bytes to mean the same thing.
///
/// `exception_pointers` is an address inside the crashing process. The monitor passes it to
/// `MiniDumpWriteDump` with `ClientPointers = TRUE`, which reads it from the target. For an
/// init-failure (`kind == KIND_INIT_FAILURE`) there is no exception; the `reason`/`reason_len`
/// bytes carry a human-readable cause instead, and no dump is written.
#[repr(C)]
#[derive(Clone, Copy)]
struct CrashInfo {
    thread_id: u32,
    kind: u32,
    exception_pointers: u64,
    reason_len: u32,
    reason: [u8; REASON_CAPACITY],
}

/// A layer's registration with the monitor.
#[derive(bincode::Encode, bincode::Decode, Debug, Clone)]
pub struct Registration {
    /// The registering process's id.
    pub pid: u32,
    /// Its parent's id.
    pub parent_pid: u32,
    /// Its image name.
    pub name: String,
    /// Its session role label.
    pub role: String,
    /// The shared incident stem the monitor reuses for every artifact file name, so the in-process
    /// record and the monitor's dump/report/modules files cluster together. The layer fills it in
    /// `crash::register_monitor`.
    pub stem: String,
    /// Its layer log's file name, inside the session directory, when file logging is active and
    /// writes there.
    ///
    /// Registered so a crash bundles this precise file, never a pid-glob that could match a prior
    /// session's log. Only the name travels: the monitor already knows the directory, and it opens
    /// nothing else.
    pub log_name: Option<String>,
    /// A startup problem reported on this process's behalf.
    ///
    /// Set only by [`report_init_failure_for`]. The monitor reports it at once and watches
    /// nothing: the layer in that process never got as far as talking to the monitor.
    pub init_report: Option<InitReport>,
}

/// A startup problem a launcher saw in a process it created. See [`report_init_failure_for`].
#[derive(bincode::Encode, bincode::Decode, Debug, Clone, PartialEq, Eq)]
pub enum InitReport {
    /// mirrord could not be set up in the process. A crash-level incident, with the dialog.
    Failed(String),
    /// The layer did not report ready in time, but nothing says it failed. A note in the
    /// session's files: no dialog, and the session is not marked as crashed for it.
    SlowStart(String),
}

impl InitReport {
    /// The text the report shows.
    pub fn reason(&self) -> &str {
        match self {
            Self::Failed(reason) | Self::SlowStart(reason) => reason,
        }
    }
}

/// Reads the init-failure reason out of the shared section.
fn read_reason(info: &CrashInfo) -> String {
    let len = (info.reason_len as usize).min(REASON_CAPACITY);
    let bytes = info.reason.get(..len).unwrap_or(&[]);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Encodes a registration as the length-prefixed bincode message the monitor reads.
fn encode_registration(registration: &Registration) -> io::Result<Vec<u8>> {
    let bytes = bincode::encode_to_vec(registration, bincode::config::standard())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    // The monitor refuses anything larger, so there is no point sending it.
    if bytes.len() > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "registration message too large",
        ));
    }

    let mut message = Vec::with_capacity(4 + bytes.len());
    message.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    message.extend_from_slice(&bytes);
    Ok(message)
}

/// Reads a length-prefixed bincode registration.
pub fn read_registration(stream: &mut TcpStream) -> io::Result<Registration> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "registration message too large",
        ));
    }

    let mut bytes = vec![0u8; length];
    stream.read_exact(&mut bytes)?;
    let (registration, _) = bincode::decode_from_slice(&bytes, bincode::config::standard())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(registration)
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    fn registration(init_failure: Option<&str>) -> Registration {
        Registration {
            pid: 4242,
            parent_pid: 7,
            name: "child.exe".to_owned(),
            role: "child".to_owned(),
            stem: "stem".to_owned(),
            log_name: None,
            init_report: init_failure.map(|reason| InitReport::Failed(reason.to_owned())),
        }
    }

    /// The reason has to arrive with the registration, because the process it is about may be
    /// gone by the time the monitor reads it.
    #[test]
    fn an_init_failure_travels_in_the_registration() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");

        let monitor = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let received = read_registration(&mut stream).expect("read the registration");
            stream.write_all(&[ACK_READY]).expect("ack");
            received
        });

        let ack = exchange(
            address,
            &registration(Some("the layer failed")),
            Duration::from_secs(1),
            Duration::from_secs(5),
        )
        .expect("exchange");
        let received = monitor.join().expect("monitor thread");

        assert_eq!(ack, ACK_READY);
        assert_eq!(
            received.init_report,
            Some(InitReport::Failed("the layer failed".to_owned()))
        );
        assert_eq!(received.pid, 4242);
    }

    /// A registration the monitor would refuse is not sent at all.
    #[test]
    fn an_oversized_registration_is_refused_before_sending() {
        let mut oversized = registration(Some(&"x".repeat(MAX_MESSAGE_LEN)));
        oversized.name = "child.exe".to_owned();
        let error = encode_registration(&oversized).expect_err("too large to send");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        // Nothing listens here; the size check has to fail first.
        let unreachable = SocketAddr::from(([127, 0, 0, 1], 9));
        let error = exchange(
            unreachable,
            &oversized,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect_err("too large to send");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// The report is sent from a `CreateProcess` detour, so a monitor that accepts and then says
    /// nothing must not hold that call for longer than the budget.
    #[test]
    fn a_silent_monitor_cannot_stall_the_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let monitor = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            // Hold the connection open, never answering, until the client has given up.
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
        });

        let started = Instant::now();
        let result = exchange(
            address,
            &registration(Some("the layer failed")),
            Duration::from_millis(500),
            Duration::from_millis(700),
        );
        let elapsed = started.elapsed();
        monitor.join().expect("monitor thread");

        assert!(result.is_err(), "a monitor that never answers is an error");
        assert!(
            elapsed < Duration::from_secs(2),
            "the budget bounds the whole exchange, took {elapsed:?}"
        );
    }

    fn info_with(reason: &[u8], reason_len: u32) -> CrashInfo {
        let mut buffer = [0u8; REASON_CAPACITY];
        let take = reason.len().min(REASON_CAPACITY);
        if let (Some(dst), Some(src)) = (buffer.get_mut(..take), reason.get(..take)) {
            dst.copy_from_slice(src);
        }
        CrashInfo {
            thread_id: 0,
            kind: KIND_INIT_FAILURE,
            exception_pointers: 0,
            reason_len,
            reason: buffer,
        }
    }

    #[test]
    fn read_reason_reads_the_written_prefix() {
        let text = b"parent pid=19580 (node.exe) dead";
        assert_eq!(
            read_reason(&info_with(text, text.len() as u32)),
            "parent pid=19580 (node.exe) dead"
        );
    }

    #[test]
    fn read_reason_clamps_an_oversized_length() {
        // A length past the buffer must clamp to the capacity, not read out of bounds.
        let reason = read_reason(&info_with(b"hello", u32::MAX));
        assert_eq!(reason.len(), REASON_CAPACITY);
        assert!(reason.starts_with("hello"));
    }

    #[test]
    fn read_reason_is_lossy_on_invalid_utf8() {
        let bytes = [0xFF, 0xFE, b'x'];
        let reason = read_reason(&info_with(&bytes, bytes.len() as u32));
        assert!(reason.ends_with('x'));
    }

    #[test]
    fn read_reason_zero_length_is_empty() {
        assert_eq!(read_reason(&info_with(b"ignored", 0)), "");
    }
}

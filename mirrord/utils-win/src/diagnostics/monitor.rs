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
//! [`CrashInfo`] shared-section layout, the [`Registration`] wire format, and its codec. The two
//! sides live in submodules: [`channel`] is what a registered layer holds and signals through,
//! [`server`] is the monitor's accept-and-watch loop.

use std::{
    io::{self, Read, Write},
    net::TcpStream,
};

mod channel;
mod server;

pub use channel::{MonitorChannel, register};
pub use server::{MonitorConfig, serve};

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
const ACK_READY: u8 = 1;
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
    /// `install`.
    pub stem: String,
    /// Its exact layer log file, when file logging is active.
    ///
    /// Registered so a crash bundles this precise file, never a pid-glob that could match a prior
    /// session's log.
    pub log_path: Option<String>,
}

/// Reads the init-failure reason out of the shared section.
fn read_reason(info: &CrashInfo) -> String {
    let len = (info.reason_len as usize).min(REASON_CAPACITY);
    let bytes = info.reason.get(..len).unwrap_or(&[]);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Writes a length-prefixed bincode registration.
fn write_registration(stream: &mut TcpStream, registration: &Registration) -> io::Result<()> {
    let bytes = bincode::encode_to_vec(registration, bincode::config::standard())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let length = (bytes.len() as u32).to_le_bytes();
    stream.write_all(&length)?;
    stream.write_all(&bytes)?;
    stream.flush()
}

/// Reads a length-prefixed bincode registration.
fn read_registration(stream: &mut TcpStream) -> io::Result<Registration> {
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
    use super::*;

    fn info_with(reason: &[u8], reason_len: u32) -> CrashInfo {
        let mut buffer = [0u8; REASON_CAPACITY];
        let take = reason.len().min(REASON_CAPACITY);
        buffer[..take].copy_from_slice(&reason[..take]);
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

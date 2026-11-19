//! The Linux and macOS side of the runtime pause: a Unix socket for the port, in the same private
//! directory as the release FIFOs, and a wait on the FIFO's readability.

use std::{
    fs::{self, Permissions},
    io,
    os::unix::fs::{FileTypeExt, PermissionsExt},
};

use nix::{
    errno::Errno,
    sys::signal::kill,
    unistd::{Pid, UnlinkatFlags, unlinkat},
};
use tokio::{
    io::{Interest, unix::AsyncFd},
    net::{UnixListener, UnixStream},
};
use uuid::Uuid;

use super::PortError;
use crate::{
    Gate, GateError, HeldProcess, HoldError, HoldOutcome, ReleaseGate,
    unix::{PrivateDir, private_dir, runtime_dir},
};

/// How the socket's name starts. `mirrord_config::debug_hold::dotnet_ports` recognizes it, in the
/// private `mirrord-debug-<uid>` directory, to drop a stale entry of an outer `mirrord up`.
const SOCKET_PREFIX: &str = "dotnet-";

/// The size of `sockaddr_un`'s `sun_path`, its terminating NUL included.
const SUN_PATH: usize = if cfg!(target_os = "macos") { 104 } else { 108 };

/// The port's socket, `<runtime dir>/mirrord-debug-<uid>/dotnet-<up's pid>-<uuid>.sock`.
pub(super) struct Listener {
    listener: UnixListener,
    _socket: Socket,
}

/// The socket's file, removed with the listener.
struct Socket {
    dir: PrivateDir,
    name: String,
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let Err(error) = unlinkat(&self.dir.fd, self.name.as_str(), UnlinkatFlags::NoRemoveDir) {
            tracing::debug!(%error, name = self.name, "failed to remove the .NET diagnostic port");
        }
    }
}

impl Listener {
    /// Binds a new socket in the private directory, after removing those of `mirrord up`s that
    /// are gone.
    ///
    /// The directory is this user's alone, and the socket is `0600` too. Binding refuses a path
    /// that exists, so nothing else can be serving it.
    pub(super) fn create() -> Result<(Self, String), PortError> {
        let dir = private_dir(&runtime_dir()).map_err(PortError::Directory)?;
        remove_stale_sockets(&dir);
        let name = format!(
            "{SOCKET_PREFIX}{}-{}.sock",
            std::process::id(),
            Uuid::new_v4().simple()
        );
        let path = dir.path.join(&name);
        let address = path
            .to_str()
            .ok_or_else(|| PortError::UnusablePath(dir.path.clone()))?
            .to_owned();
        // Never truncated: a runtime would connect to another path.
        if address.len() >= SUN_PATH {
            return Err(PortError::PathTooLong {
                path: address,
                max: SUN_PATH - 1,
            });
        }

        let failed = |source| PortError::Create {
            address: address.clone(),
            source,
        };
        let listener = UnixListener::bind(&path).map_err(failed)?;
        let socket = Socket { dir, name };
        fs::set_permissions(&path, Permissions::from_mode(0o600)).map_err(failed)?;
        Ok((
            Self {
                listener,
                _socket: socket,
            },
            address,
        ))
    }

    /// Waits for a runtime to connect.
    ///
    /// # Returns
    ///
    /// The connection, and the pid of the process at its other end (`SO_PEERCRED` on Linux,
    /// `LOCAL_PEERPID` on macOS), when the system says.
    pub(super) async fn accept(&mut self) -> io::Result<(UnixStream, Option<u32>)> {
        let (stream, _) = self.listener.accept().await?;
        let pid = stream
            .peer_cred()
            .ok()
            .and_then(|credentials| credentials.pid())
            .and_then(|pid| u32::try_from(pid).ok());
        Ok((stream, pid))
    }
}

/// Removes the sockets in `dir` of `mirrord up`s that are gone.
///
/// A `mirrord up` that's killed never removes its sockets, so the next one does. The pid in the
/// name is `up`'s own, not a runtime's.
fn remove_stale_sockets(dir: &PrivateDir) {
    let Ok(entries) = fs::read_dir(&dir.path) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(socket_owner) else {
            continue;
        };
        let is_socket = entry.file_type().is_ok_and(|kind| kind.is_socket());
        // Probing with no signal: `ESRCH` means no process has the pid.
        if is_socket && matches!(kill(pid, None), Err(Errno::ESRCH)) {
            // Through the checked directory's descriptor, so only an entry inside it goes.
            if let Err(error) = unlinkat(&dir.fd, name.as_os_str(), UnlinkatFlags::NoRemoveDir) {
                tracing::debug!(%error, ?name, "failed to remove a stale .NET diagnostic port");
            }
        }
    }
}

/// The pid of the `mirrord up` a socket named `dotnet-<pid>-<uuid>.sock` was made by, or `None`
/// for any other name. Never 0 or below, which `kill` reads as process groups.
fn socket_owner(name: &str) -> Option<Pid> {
    let (pid, _rest) = name
        .strip_prefix(SOCKET_PREFIX)?
        .strip_suffix(".sock")?
        .split_once('-')?;
    let pid = pid.parse::<i32>().ok().filter(|pid| *pid > 0)?;
    Some(Pid::from_raw(pid))
}

/// Nothing: on Unix the release's wait watches only the FIFO, and a held runtime that exits is
/// seen by its connection ending.
pub(super) struct HeldRuntime;

/// What the announcement says about a paused runtime's process.
pub(super) fn open(pid: u32) -> io::Result<(HeldRuntime, HeldProcess)> {
    Ok((
        HeldRuntime,
        HeldProcess {
            image: image_of(pid),
            parent_pid: parent_of(pid),
            command_line: None,
        },
    ))
}

#[cfg(target_os = "linux")]
fn image_of(pid: u32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|image| image.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn image_of(pid: u32) -> Option<String> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buffer` holds as many bytes as the call is told it may write.
    let written = unsafe {
        libc::proc_pidpath(
            pid,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    let written = usize::try_from(written)
        .ok()
        .filter(|written| *written > 0)?;
    buffer.truncate(written);
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn image_of(_pid: u32) -> Option<String> {
    None
}

/// The parent's pid, the field after the state in `/proc/<pid>/stat`. The command name before
/// them is in parentheses and may hold anything, so the fields are read after its last `)`.
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    fields.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
}

/// Waits for the IDE's release byte, watching the FIFO's read end without blocking a thread.
pub(super) async fn released(
    pid: u32,
    gate: Gate,
    _held: HeldRuntime,
) -> Result<HoldOutcome, HoldError> {
    let release = gate.release_name().to_owned();
    let watch_failed = |source| HoldError::Wait {
        pid,
        source: GateError::Watch {
            path: release.clone().into(),
            source,
        },
    };
    // SAFETY: the gate owns the FIFO's read end, which it returns from `as_raw_fd` every time,
    // and closes only when it's dropped, with this `AsyncFd`.
    let watched = unsafe { AsyncFd::register_with_interest(gate, Interest::READABLE) }
        .map_err(|error| watch_failed(error.into()))?;
    loop {
        let mut ready = watched.readable().await.map_err(watch_failed)?;
        match watched.get_ref().try_take_release() {
            Ok(true) => return Ok(HoldOutcome::Released),
            Ok(false) => ready.clear_ready(),
            Err(source) => return Err(HoldError::Wait { pid, source }),
        }
    }
}

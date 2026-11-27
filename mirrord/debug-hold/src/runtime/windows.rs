//! The Windows side of the runtime pause: a named pipe for the port, and a thread that waits on
//! the release event.
//!
//! The runtime opens `\\.\pipe\<address>`, adding the prefix itself, so the address is the pipe's
//! bare name.

use std::{
    io,
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    time::Duration,
};

use tokio::{
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::oneshot,
};
use utils_win::security::CurrentUserSecurityAttributes;
use uuid::Uuid;
use winapi::{
    shared::minwindef::FALSE,
    um::{
        processthreadsapi::OpenProcess,
        winbase::GetNamedPipeClientProcessId,
        winnt::{PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE},
    },
};

use super::PortError;
use crate::{Gate, HeldProcess, HoldError, HoldOutcome, ReleaseGate};

/// How the pipe's name starts. `mirrord_config::debug_hold::dotnet_ports` recognizes it, to drop a
/// stale entry of an outer `mirrord up`.
const PIPE_PREFIX: &str = "mirrord-dotnet-";

/// How long to wait before trying again to create a pipe instance, without which a runtime that
/// connects waits until one exists.
const RETRY_CREATE: Duration = Duration::from_secs(1);

/// The port's pipe: always one instance waiting for the next runtime.
pub(super) struct Listener {
    /// `\\.\pipe\<address>`.
    path: String,
    /// The DACL every instance is created with: the user running `mirrord up`, and nobody else.
    security: CurrentUserSecurityAttributes,
    /// The instance the next runtime connects to.
    next: NamedPipeServer,
}

impl Listener {
    /// Creates the pipe's first instance, at a new name.
    ///
    /// The first instance refuses a name that exists, so nothing else can be serving it, and
    /// every instance refuses clients on other machines.
    pub(super) fn create() -> Result<(Self, String), PortError> {
        let address = format!("{PIPE_PREFIX}{}", Uuid::new_v4().simple());
        let path = format!(r"\\.\pipe\{address}");
        let failed = |source| PortError::Create {
            address: address.clone(),
            source,
        };
        let security = CurrentUserSecurityAttributes::new().map_err(failed)?;
        let next = create_instance(&path, &security, Instance::First).map_err(failed)?;
        Ok((
            Self {
                path,
                security,
                next,
            },
            address,
        ))
    }

    /// Waits for a runtime to connect.
    ///
    /// # Returns
    ///
    /// The connection, and the pid of the process at its other end, when Windows says.
    pub(super) async fn accept(&mut self) -> io::Result<(NamedPipeServer, Option<u32>)> {
        let connected = self.next.connect().await;
        // The next instance comes first, so a runtime that reconnects at once finds one. An
        // instance whose client left before it connected can't be reused either.
        let next = loop {
            match create_instance(&self.path, &self.security, Instance::Next) {
                Ok(next) => break next,
                Err(error) => {
                    tracing::warn!(%error, path = self.path, "failed to create another instance of the .NET diagnostic port; retrying");
                    tokio::time::sleep(RETRY_CREATE).await;
                }
            }
        };
        let instance = std::mem::replace(&mut self.next, next);
        connected?;
        let pid = client_pid(&instance);
        Ok((instance, pid))
    }
}

/// Which of the pipe's instances [`create_instance`] creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Instance {
    /// The first, which refuses a name that already exists.
    First,
    /// One after the first, at the name the first took.
    Next,
}

fn create_instance(
    path: &str,
    security: &CurrentUserSecurityAttributes,
    instance: Instance,
) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(instance == Instance::First)
        .reject_remote_clients(true);
    // SAFETY: `security` lives through the call, which copies its descriptor into the pipe.
    unsafe { options.create_with_security_attributes_raw(path, security.as_raw()) }
}

/// The pid of the process at a connected pipe's other end.
fn client_pid(pipe: &NamedPipeServer) -> Option<u32> {
    let mut pid = 0;
    // SAFETY: the pipe's handle is open for the borrow, and `pid` is a `u32` the call writes.
    let read = unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle().cast(), &mut pid) };
    (read != 0).then_some(pid)
}

/// A held runtime's process, which the release's wait also watches.
pub(super) struct HeldRuntime {
    /// With `SYNCHRONIZE`, to wait on, and `PROCESS_QUERY_LIMITED_INFORMATION`, for its image.
    process: OwnedHandle,
}

/// Opens a paused runtime's process, and reads what the announcement says about it.
pub(super) fn open(pid: u32) -> io::Result<(HeldRuntime, HeldProcess)> {
    // SAFETY: no pointers; a null handle is checked below.
    let process =
        unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle this function owns.
    let process = unsafe { OwnedHandle::from_raw_handle(process.cast()) };
    let facts = HeldProcess {
        image: utils_win::process::process_image_path(process.as_handle()),
        parent_pid: None,
        command_line: None,
    };
    Ok((HeldRuntime { process }, facts))
}

/// Waits for the IDE's release, or for the process to end.
///
/// The event's wait blocks, so it runs on a thread of its own. The thread always ends: when the
/// IDE releases, or when the process ends, which `mirrord up` makes sure of as it stops, since it
/// ends every process of the service.
pub(super) async fn released(
    pid: u32,
    gate: Gate,
    held: HeldRuntime,
) -> Result<HoldOutcome, HoldError> {
    let (outcome_tx, outcome_rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = outcome_tx.send(gate.wait(held.process.as_handle()));
    });
    // Dropped unsent only if the thread panicked.
    outcome_rx
        .await
        .unwrap_or(Err(HoldError::WaitAbandoned { pid }))
}

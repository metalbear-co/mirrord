//! The .NET runtime's startup pause, served by `mirrord up` for each debugged service.
//!
//! A .NET runtime whose `DOTNET_DiagnosticPorts` names `<address>,connect,suspend` connects to
//! that address at its startup, and waits before any managed code runs until it's told to go on.
//! [`DiagnosticPort`] is that address's server. For each runtime that pauses on it, it holds the
//! runtime the way a process is held: a [`Gate`] only the IDE can release, an
//! [`Announcement`] with `"trigger":"runtime"`, and once the IDE releases it, `ResumeRuntime`.
//!
//! The runtime connects to the server, not the other way round, and every command consumes a
//! connection: the runtime answers, closes it, connects again and re-sends its advertise. So one
//! paused runtime goes, connection by connection:
//!
//! 1. ProcessInfo3, for its entry assembly, with ProcessInfo2 and then ProcessInfo on the
//!    following connections when a runtime can't answer it.
//! 2. Kept idle while it's held, then `ResumeRuntime` once the IDE releases it.
//! 3. Kept idle until the process exits, so the runtime doesn't reconnect in a loop.
//!
//! The runtime reconnects the moment a connection closes, often before its reply is even read.
//! So each runtime gets one task that takes its connections in order, and nothing about a runtime
//! is decided on a connection before the reply of the one before it is read.
//!
//! The SDK's own processes pause too: under `dotnet run` the SDK itself, and a build's compiler
//! server and nodes. They're resumed at once, with no announcement, by their entry assembly
//! ([`is_sdk_tooling`]).
//!
//! The listener and the gate's wait differ by platform: a named pipe and a thread waiting on the
//! release event on Windows, a Unix socket and the release FIFO's readability elsewhere.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::mpsc,
};
use uuid::Uuid;

pub use self::ipc::IpcError;
use self::ipc::{ADVERTISE_SIZE, Advertise, Command, ProcessInfo};
use crate::{
    Announcement, Ended, EndedOutcome, Gate, HoldError, HoldOutcome, ReleaseGate, RuntimeFacts,
};

mod ipc;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use self::unix as platform;
#[cfg(windows)]
use self::windows as platform;

/// The entry assemblies of the .NET SDK's own processes, as ProcessInfo3 reports them at the
/// pause: `dotnet` for the SDK under `dotnet run` and `dotnet build`, `MSBuild` for a build node,
/// `VBCSCompiler` for the compiler server, `csc` and `vbc` for a compiler run without it, and
/// `dotnet-watch` for `dotnet watch`'s watcher, whose app is held as usual.
///
/// NOTE: an app whose own entry assembly has one of these names is resumed too. The SDK runs in
/// `dotnet` itself, so its command line names no SDK path to tell them apart by.
pub const SDK_TOOLING: &[&str] = &[
    "dotnet",
    "MSBuild",
    "VBCSCompiler",
    "csc",
    "vbc",
    "dotnet-watch",
];

/// Whether a runtime whose entry assembly is `entry_assembly` is one of the SDK's own processes,
/// which nobody debugs. Matched without case, as assembly names are.
pub fn is_sdk_tooling(entry_assembly: &str) -> bool {
    SDK_TOOLING
        .iter()
        .any(|tooling| tooling.eq_ignore_ascii_case(entry_assembly))
}

/// What happened to a runtime on the port, for `mirrord up` to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// A runtime is held, and the IDE may attach to it, then release it.
    Held(Announcement),
    /// A held runtime's hold is over.
    Ended(Ended),
    /// One of the SDK's own runtimes was resumed without a hold.
    Skipped {
        /// Its process.
        pid: u32,
        /// Its entry assembly.
        entry_assembly: String,
    },
    /// A connection or a runtime was refused, or a runtime couldn't be held and runs on without
    /// the debugger.
    Refused(String),
}

/// How long each step of the protocol may take. Only tests change it.
#[derive(Debug, Clone, Copy)]
struct Timing {
    /// For a connection's advertise, which a runtime sends at once.
    advertise: Duration,
    /// For the runtime's reply to a command.
    reply: Duration,
    /// For the runtime to connect again once a connection ended. It does so at once, retrying
    /// with a backoff of at most half a second, so a runtime that doesn't is gone.
    reconnect: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            advertise: Duration::from_secs(5),
            reply: Duration::from_secs(10),
            reconnect: Duration::from_secs(5),
        }
    }
}

/// Why the diagnostic port couldn't be created.
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("failed to create the .NET diagnostic port {address}")]
    Create {
        address: String,
        #[source]
        source: std::io::Error,
    },

    #[cfg(unix)]
    #[error("failed to prepare the directory of the .NET diagnostic port")]
    Directory(#[source] crate::GateError),

    #[cfg(unix)]
    #[error(
        "the .NET diagnostic port's path {path} is longer than a Unix socket's path may be ({max} bytes); set XDG_RUNTIME_DIR to a shorter directory"
    )]
    PathTooLong { path: String, max: usize },

    #[cfg(unix)]
    #[error("the .NET diagnostic port's directory {0} isn't UTF-8")]
    UnusablePath(std::path::PathBuf),
}

/// The diagnostic port of one debugged service, which its .NET runtimes pause on.
///
/// Only processes of the user running `mirrord up` can connect to it: a pipe with a DACL of that
/// user alone on Windows, a socket in a `0700` directory elsewhere. A connection whose peer isn't
/// the process its advertise names is refused, so no process can get another one attached.
pub struct DiagnosticPort {
    listener: platform::Listener,
    address: String,
}

impl DiagnosticPort {
    /// Creates a port at a new, unguessable address. Must be called within a tokio runtime.
    pub fn create() -> Result<Self, PortError> {
        let (listener, address) = platform::Listener::create()?;
        Ok(Self { listener, address })
    }

    /// The address that goes in `DOTNET_DiagnosticPorts`: the pipe's name, without `\\.\pipe\`,
    /// on Windows, the socket's path elsewhere.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Serves the port until the future is dropped, telling `report` about each runtime.
    ///
    /// Dropping it closes the port. A runtime held at that moment stays paused: `mirrord up` drops
    /// the port only as it ends the service's processes.
    pub async fn serve(self, report: impl Fn(Report) + Send + Sync + 'static) {
        self.serve_with(Timing::default(), Arc::new(report)).await;
    }

    async fn serve_with(mut self, timing: Timing, report: Arc<dyn Fn(Report) + Send + Sync>) {
        let runtimes = Runtimes::default();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (connection, peer) = match self.listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::debug!(%error, "a connection to the .NET diagnostic port failed");
                    continue;
                }
            };
            let runtimes = runtimes.clone();
            let report = report.clone();
            // Every connection's task, and each runtime's, lives in `tasks`, so dropping the
            // future ends them all.
            tasks.spawn(async move {
                if let Some((advertise, connection)) =
                    handshake(connection, peer, timing, &*report).await
                    && let Some(runtime) = runtimes.route(advertise, connection, timing, report)
                {
                    runtime.await;
                }
            });
            while tasks.try_join_next().is_some() {}
        }
    }
}

/// Reads a connection's advertise and checks that its peer is the process it names.
async fn handshake<C: AsyncRead + Unpin>(
    mut connection: C,
    peer: Option<u32>,
    timing: Timing,
    report: &(dyn Fn(Report) + Send + Sync),
) -> Option<(Advertise, C)> {
    let mut frame = [0; ADVERTISE_SIZE];
    let read = tokio::time::timeout(
        timing.advertise,
        ipc::read_exactly(&mut connection, &mut frame),
    )
    .await
    .unwrap_or(Err(IpcError::TimedOut(timing.advertise)));
    let advertise = match read.and_then(|()| ipc::parse_advertise(&frame)) {
        Ok(advertise) => advertise,
        Err(error) => {
            report(Report::Refused(format!(
                "a connection to the .NET diagnostic port sent no advertise ({error}), so it was closed"
            )));
            return None;
        }
    };
    if peer != Some(advertise.pid) {
        let peer = peer.map_or_else(
            || "an unknown process".to_owned(),
            |pid| format!("process {pid}"),
        );
        report(Report::Refused(format!(
            "{peer} connected to the .NET diagnostic port as process {}, so it was refused",
            advertise.pid
        )));
        return None;
    }
    Some((advertise, connection))
}

/// What the port knows of each runtime, by its cookie.
enum Slot<C> {
    /// Its task runs, and takes its connections from this channel.
    Active(mpsc::UnboundedSender<C>),
    /// It was resumed. Its connections from now on are only kept idle, never held again.
    Resumed,
}

/// Every runtime the port has seen, shared by the connections' tasks.
struct Runtimes<C>(Arc<Mutex<HashMap<Uuid, Slot<C>>>>);

impl<C> Clone for Runtimes<C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<C> Default for Runtimes<C> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

impl<C: AsyncRead + AsyncWrite + Unpin + Send + 'static> Runtimes<C> {
    /// Hands a checked connection to its runtime's task.
    ///
    /// # Returns
    ///
    /// What the caller runs for this connection: the runtime's task for a new runtime, an idle
    /// wait for a resumed one, and nothing for one whose task takes it.
    fn route(
        &self,
        advertise: Advertise,
        connection: C,
        timing: Timing,
        report: Arc<dyn Fn(Report) + Send + Sync>,
    ) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
        let mut slots = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match slots.get(&advertise.cookie) {
            // A task that just ended drops this connection unread; the runtime reconnects, and
            // finds the slot the task left.
            Some(Slot::Active(connections)) => {
                let _ = connections.send(connection);
                None
            }
            Some(Slot::Resumed) => Some(Box::pin(idle_until_closed(connection))),
            None => {
                let (connections_tx, connections_rx) = mpsc::unbounded_channel();
                slots.insert(advertise.cookie, Slot::Active(connections_tx));
                let runtimes = self.clone();
                Some(Box::pin(async move {
                    let connections = Connections {
                        rx: connections_rx,
                        reconnect: timing.reconnect,
                    };
                    let resumed =
                        serve_runtime(advertise, connection, connections, timing, &*report).await;
                    runtimes.finish(advertise.cookie, resumed);
                }))
            }
        }
    }

    /// Records how a runtime's task ended. A runtime that was resumed is never held again,
    /// whatever it sends; one that wasn't is forgotten, and starts over if it ever reconnects.
    fn finish(&self, cookie: Uuid, resumed: bool) {
        let mut slots = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if resumed {
            slots.insert(cookie, Slot::Resumed);
        } else {
            slots.remove(&cookie);
        }
    }
}

/// A runtime's connections after its first, in the order it made them.
struct Connections<C> {
    /// The port's router --a runtime's next connection--> its task.
    rx: mpsc::UnboundedReceiver<C>,
    reconnect: Duration,
}

impl<C> Connections<C> {
    /// The runtime's next connection, or `None` when it didn't reconnect in time, so it's gone.
    async fn next(&mut self) -> Option<C> {
        tokio::time::timeout(self.reconnect, self.rx.recv())
            .await
            .ok()
            .flatten()
    }
}

/// Holds one runtime, from its first connection until it exits.
///
/// # Returns
///
/// Whether the runtime was resumed, so it must never be held again.
async fn serve_runtime<C: AsyncRead + AsyncWrite + Unpin>(
    advertise: Advertise,
    mut connection: C,
    mut connections: Connections<C>,
    timing: Timing,
    report: &(dyn Fn(Report) + Send + Sync),
) -> bool {
    let pid = advertise.pid;

    // 1. What it runs. Each command consumes its connection, so each try waits for the next one.
    let mut info = None;
    for command in [
        Command::ProcessInfo3,
        Command::ProcessInfo2,
        Command::ProcessInfo,
    ] {
        let reply = ipc::request(&mut connection, command, timing.reply)
            .await
            .and_then(|payload| ipc::parse_process_info(command, &payload));
        drop(connection);
        connection = match connections.next().await {
            Some(next) => next,
            None => return false,
        };
        match reply {
            Ok(reply) => {
                info = Some(reply);
                break;
            }
            Err(error) => {
                tracing::debug!(pid, ?command, %error, "a paused runtime didn't say what it runs")
            }
        }
    }
    let info = info.unwrap_or_default();

    // 2. The SDK's own processes go on at once.
    if let Some(tooling) = info
        .entry_assembly
        .as_deref()
        .filter(|name| is_sdk_tooling(name))
    {
        let tooling = tooling.to_owned();
        if resume(connection, &mut connections, timing).await.is_err() {
            return false;
        }
        report(Report::Skipped {
            pid,
            entry_assembly: tooling,
        });
        idle_until_gone(&mut connections).await;
        return true;
    }

    // 3. Everything else is held until the IDE releases it.
    let resumed = match hold(&advertise, info, connection, &mut connections, report).await {
        Held::Released {
            connection,
            release,
        } => match resume(connection, &mut connections, timing).await {
            Ok(()) => {
                report(Report::Ended(Ended::new(
                    pid,
                    release,
                    EndedOutcome::Resumed,
                    None,
                )));
                true
            }
            Err(Resume::Gone) => {
                report(Report::Ended(Ended::new(
                    pid,
                    release,
                    EndedOutcome::Exited,
                    None,
                )));
                return false;
            }
            Err(Resume::Refused(error)) => {
                report(Report::Ended(Ended::new(
                    pid,
                    release,
                    EndedOutcome::Failed,
                    Some(error.to_string()),
                )));
                return false;
            }
        },
        Held::Exited { release } => {
            report(Report::Ended(Ended::new(
                pid,
                release,
                EndedOutcome::Exited,
                None,
            )));
            return false;
        }
        Held::Failed { release, error } => {
            report(Report::Ended(Ended::new(
                pid,
                release,
                EndedOutcome::Failed,
                Some(error.to_string()),
            )));
            return false;
        }
        Held::CannotHold { connection, why } => {
            report(Report::Refused(format!(
                "process {pid}'s .NET runtime couldn't be held ({why}), so it runs without the debugger"
            )));
            resume(connection, &mut connections, timing).await.is_ok()
        }
    };

    // 4. Resumed: its connections are kept idle until it exits.
    if resumed {
        idle_until_gone(&mut connections).await;
    }
    resumed
}

/// How a hold of a runtime ended.
enum Held<C> {
    /// The IDE released it, and `connection` is the one to resume it on.
    Released { connection: C, release: String },
    /// The process ended while held.
    Exited { release: String },
    /// Waiting for the release failed, so the runtime stays paused.
    Failed { release: String, error: HoldError },
    /// It couldn't be held at all, so it should be resumed on `connection` at once.
    CannotHold { connection: C, why: String },
}

/// Holds a paused runtime: creates its gate, announces it, and waits for the IDE's release while
/// keeping `connection` idle.
///
/// A connection that ends means the process ended, unless the runtime connects again: it keeps
/// one open to every port, and closes none while paused.
async fn hold<C: AsyncRead + Unpin>(
    advertise: &Advertise,
    info: ProcessInfo,
    mut connection: C,
    connections: &mut Connections<C>,
    report: &(dyn Fn(Report) + Send + Sync),
) -> Held<C> {
    let pid = advertise.pid;
    let (held, process) = match platform::open(pid) {
        Ok(opened) => opened,
        Err(error) => {
            return Held::CannotHold {
                connection,
                why: format!("its process couldn't be opened: {error}"),
            };
        }
    };
    let gate = match Gate::create(pid) {
        Ok(gate) => gate,
        Err(error) => {
            return Held::CannotHold {
                connection,
                why: error.to_string(),
            };
        }
    };
    let release = gate.release_name().to_owned();
    let mut process = process;
    if process.command_line.is_none() {
        process.command_line = info.command_line;
    }
    report(Report::Held(Announcement::runtime(
        pid,
        release.clone(),
        process,
        RuntimeFacts {
            runtime: Some(advertise.cookie.to_string()),
            entry_assembly: info.entry_assembly,
            runtime_version: info.runtime_version,
            os: info.os,
            arch: info.arch,
            rid: info.rid,
        },
    )));
    tracing::debug!(
        pid,
        release,
        "debug hold: a paused .NET runtime waits for the release"
    );

    let released = platform::released(pid, gate, held);
    tokio::pin!(released);
    loop {
        tokio::select! {
            outcome = &mut released => {
                return match outcome {
                    Ok(HoldOutcome::Released) => Held::Released { connection, release },
                    Ok(HoldOutcome::ProcessExited) => Held::Exited { release },
                    Err(error) => Held::Failed { release, error },
                };
            }
            () = closed(&mut connection) => match connections.next().await {
                Some(next) => connection = next,
                None => return Held::Exited { release },
            },
        }
    }
}

/// Why a runtime couldn't be resumed.
enum Resume {
    /// Its connections ended and it never came back: it's gone.
    Gone,
    /// It answered `ResumeRuntime` with an error, so it stays paused.
    Refused(IpcError),
}

/// Sends `ResumeRuntime`, on later connections too while they keep breaking.
///
/// Done once the runtime answers OK, without waiting for it to connect again: a program that
/// ends at once may never do so.
async fn resume<C: AsyncRead + AsyncWrite + Unpin>(
    mut connection: C,
    connections: &mut Connections<C>,
    timing: Timing,
) -> Result<(), Resume> {
    loop {
        let resumed = ipc::request(&mut connection, Command::ResumeRuntime, timing.reply).await;
        drop(connection);
        match resumed {
            Ok(_) => return Ok(()),
            Err(error @ IpcError::Refused { .. }) => return Err(Resume::Refused(error)),
            Err(error) => {
                tracing::debug!(%error, "ResumeRuntime didn't reach the runtime; trying its next connection");
                connection = connections.next().await.ok_or(Resume::Gone)?;
            }
        }
    }
}

/// Keeps a resumed runtime's connections idle until it stops reconnecting, which it does only
/// when it exits.
async fn idle_until_gone<C: AsyncRead + Unpin>(connections: &mut Connections<C>) {
    while let Some(mut connection) = connections.next().await {
        closed(&mut connection).await;
    }
}

/// Keeps a connection of a resumed runtime idle until it closes.
async fn idle_until_closed<C: AsyncRead + Unpin>(mut connection: C) {
    closed(&mut connection).await;
}

/// Resolves once the runtime closed `connection`, or broke it. A runtime sends nothing on a
/// connection it waits on, so anything it sends is ignored.
async fn closed(connection: &mut (impl AsyncRead + Unpin)) {
    let mut ignored = [0; 64];
    while matches!(connection.read(&mut ignored).await, Ok(read) if read > 0) {}
}

#[cfg(test)]
mod tests;

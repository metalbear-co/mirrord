use std::{net::SocketAddr, time::Duration};

use bincode::{Decode, Encode};

/// Environment variable used by the remote layer and workload companion to exchange accepted
/// sockets over the connection handoff socket.
pub const CONNECTION_HANDOFF_SOCKET_ENV: &str = "MIRRORD_REMOTE_HANDOFF_SOCKET";

/// Environment variable carrying the path of the admission gate's Unix socket, set by the
/// bootstrap for the workload companion.
///
/// A task opted in to admission holds its application before `main`, in the bootstrap, until it
/// knows whether a developer replaces the task. The bootstrap binds this socket (a stream socket)
/// before it spawns the companion, and the companion connects to it. The exchange goes both ways,
/// one line at a time: the companion writes [`GateMessage`] lines, first the sessions-manager's
/// verdict, and for a replaced task, later the exit code; the bootstrap answers
/// [`GateMessage::Replace`] with [`GateAck::Held`]. Without the variable the companion doesn't
/// gate the task, and the application starts as it would without admission.
pub const ADMISSION_SOCKET_ENV: &str = "MIRRORD_REMOTE_ADMISSION_SOCKET";

/// Environment variable carrying how long, in whole seconds, a task opted in to admission waits
/// for a verdict, counted from the companion connecting to [`ADMISSION_SOCKET_ENV`]. When it's
/// missing or isn't a number of seconds, the wait is [`DEFAULT_ADMISSION_TIMEOUT`].
///
/// The bootstrap starts the application once the wait is over without a verdict. The companion
/// reads the same variable and gives up a second earlier, closing the socket, so that a verdict
/// can't reach the bootstrap after it started the application. With 0 or 1, the companion gives
/// up as soon as it connects, so the task always runs its own code and can't be replaced.
///
/// For the two deadlines to keep that order, the bootstrap:
///
/// - counts its wait from `accept()` returning the companion's connection, which is never earlier
///   than the companion connecting, and bounds the wait for `accept()` itself separately;
/// - on every path that lets the application start (`continue`, the wait running out, the end of
///   the stream, a read error), closes the stream, and closes and unlinks the listener, before the
///   application starts, so that no verdict can reach it afterwards;
/// - reads the value exactly as the companion does: whole seconds, parsed as a `u64`, and
///   [`DEFAULT_ADMISSION_TIMEOUT`] for anything else.
pub const ADMISSION_TIMEOUT_ENV: &str = "MIRRORD_REMOTE_ADMISSION_TIMEOUT";

/// The wait for a verdict when [`ADMISSION_TIMEOUT_ENV`] doesn't set one.
pub const DEFAULT_ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment variable carrying the PID of the application process the bootstrap runs in, set
/// by the bootstrap for the workload companion.
///
/// The companion reads the application's command line from `/proc/<pid>/cmdline`, so a developer
/// can start a local process with the task's own arguments.
pub const APP_PID_ENV: &str = "MIRRORD_REMOTE_APP_PID";

/// Name of the variable that carries the application's command line, `argv[0]` included, as a
/// JSON array of strings, in the environment the workload companion serves to clients.
///
/// The companion reads it from `/proc/<pid>/cmdline` for the PID in [`APP_PID_ENV`], replacing
/// bytes that aren't UTF-8, and leaves the variable out when it can't. The task's own
/// environment never has it. The arguments can carry secrets, so nothing logs them.
pub const APP_ARGV_ENV: &str = "MIRRORD_REMOTE_ARGV";

/// The exit code of a replaced task whose developer session was lost before it reported the
/// local process's exit code.
///
/// Above the codes of processes killed by a signal (`128 + signal`, at most 192 on Linux), which a
/// replaced task also ends with, so a job definition can tell a lost session apart, for example
/// to retry the job with `evaluateOnExit` in AWS Batch.
pub const LOST_SESSION_EXIT_CODE: u8 = 214;

/// The exit code a replaced task ends with for the local process's exit status `status`.
///
/// A status outside `0..=255` becomes 1. `_exit` keeps only the low 8 bits of its argument, and
/// on Windows `ExitStatus::code()` can be 256 or an NTSTATUS, so truncating could end a failed
/// local process's task as a success. Shared by `mirrord up`, which reports the status, and the
/// workload companion, which relays it, so both apply one rule.
pub fn gate_exit_code(status: i32) -> u8 {
    u8::try_from(status).unwrap_or(1)
}

/// One line the workload companion writes to the bootstrap over [`ADMISSION_SOCKET_ENV`].
///
/// The line format is plain text, one message per line, in both directions. The companion writes
/// `continue`, `replace` or `exit <code>`, with the code in `0..=255`. The bootstrap writes one
/// line, `held` ([`GateAck::Held`]), as its answer to `replace`. The format is extensible: a reader
/// skips a line it doesn't know and keeps reading. What ends the wait doesn't change with that:
/// before a verdict, a timeout or the end of the stream lets the application start (failing
/// open), and after `replace`, the end of the stream ends the task with
/// [`LOST_SESSION_EXIT_CODE`].
///
/// The companion treats the task as replaced only once it read `held`. A `replace` written just
/// as the bootstrap gave up could otherwise sit unread in the socket while the application runs,
/// and the task would run both its own code and the developer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateMessage {
    /// The task runs its own code. The bootstrap loads the remote layer and lets the application
    /// start, and the companion sends nothing more.
    Continue,
    /// A developer's local process runs in place of the task: the application never starts, the
    /// bootstrap answers with [`GateAck::Held`], and then waits without a deadline for
    /// [`Self::Exit`].
    Replace,
    /// Ends a replaced task with this exit code, the local process's (see [`gate_exit_code`]) or
    /// [`LOST_SESSION_EXIT_CODE`].
    Exit(u8),
}

impl GateMessage {
    /// The line for this message, newline included.
    pub fn encode(self) -> String {
        match self {
            Self::Continue => "continue\n".to_owned(),
            Self::Replace => "replace\n".to_owned(),
            Self::Exit(code) => format!("exit {code}\n"),
        }
    }

    /// Reads one line, with or without its line ending.
    ///
    /// [`None`] for a line that isn't a message this version knows, including an `exit` whose
    /// code isn't in `0..=255`. The reader skips such a line, as the line format allows newer
    /// writers to add messages.
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim_end_matches(['\n', '\r']) {
            "continue" => Some(Self::Continue),
            "replace" => Some(Self::Replace),
            line => line
                .strip_prefix("exit ")
                .and_then(|code| code.parse().ok())
                .map(Self::Exit),
        }
    }
}

/// One line the bootstrap writes to the workload companion over [`ADMISSION_SOCKET_ENV`], in the
/// line format of [`GateMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateAck {
    /// The bootstrap's answer to [`GateMessage::Replace`]: it read it before giving up, and the
    /// application won't start.
    ///
    /// The bootstrap writes it as soon as it reads `replace`. The companion waits for it for about
    /// a second, and when it doesn't come, closes the socket and withdraws the task from the
    /// developer's session. A bootstrap that wrote `held` and then reads the end of the stream ends the task
    /// with [`LOST_SESSION_EXIT_CODE`], as for any end of the stream after `replace`.
    Held,
}

impl GateAck {
    /// The line for this message, newline included.
    pub fn encode(self) -> String {
        match self {
            Self::Held => "held\n".to_owned(),
        }
    }

    /// Reads one line, with or without its line ending.
    ///
    /// [`None`] for a line that isn't a message this version knows. The reader skips such a line,
    /// as the line format allows newer writers to add messages.
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim_end_matches(['\n', '\r']) {
            "held" => Some(Self::Held),
            _ => None,
        }
    }
}

/// Environment variable carrying the path of the remote layer library, published by the bootstrap
/// before it loads the layer.
///
/// The layer reads it once at load time, so its `execve` hook can put the layer back on
/// `LD_PRELOAD` for a child started with an environment that doesn't list it.
pub const REMOTE_LAYER_PATH_ENV: &str = "MIRRORD_REMOTE_LAYER_PATH";

/// Metadata sent alongside a transferred accepted socket fd on the connection handoff side channel.
#[derive(Encode, Decode, Debug, Eq, PartialEq, Hash, Clone)]
pub struct ConnectionHandoffRequest {
    /// Stable identifier for the accepted socket origin record.
    pub accept_id: u64,
    /// Address of the listener that accepted the connection.
    pub listener_address: SocketAddr,
    /// Address of the accepted socket on the local host.
    pub local_address: SocketAddr,
    /// Address of connection peer.
    pub peer_address: SocketAddr,
}

/// Verdict sent over the connection handoff side channel.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionHandoffVerdict {
    /// The workload-companion agent does not want to take this accepted connection, so the layer
    /// keeps the original accepted socket and falls back to local handling.
    Rejected,
    /// The workload-companion agent accepted the connection and created a local placeholder
    /// listener at this address. The layer should connect a replacement placeholder socket here
    /// and return that fd to the application instead of the original accepted socket fd.
    Accepted { placeholder_address: SocketAddr },
}

/// Response to a connection handoff request.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct ConnectionHandoffResponse {
    /// Identifier of the accepted socket origin record.
    pub accept_id: u64,
    /// Final workload-companion agent verdict for this accepted socket.
    pub verdict: ConnectionHandoffVerdict,
    /// Address of the listener that accepted the connection.
    pub listener_address: SocketAddr,
    /// Address of the accepted socket on the local host.
    pub local_address: SocketAddr,
    /// Address of connection peer.
    pub peer_address: SocketAddr,
}

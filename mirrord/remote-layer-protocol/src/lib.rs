#![cfg(unix)]

use std::net::SocketAddr;

use bincode::{Decode, Encode};

/// Environment variable used by the remote layer and workload companion to exchange accepted
/// sockets over the connection handoff socket.
pub const CONNECTION_HANDOFF_SOCKET_ENV: &str = "MIRRORD_REMOTE_HANDOFF_SOCKET";

/// Environment variable carrying the path of the admission gate's Unix socket, set by the
/// bootstrap for the workload companion.
///
/// A task opted in to admission holds its application before `main`, in the bootstrap, until it
/// knows whether a developer replaces the task. The bootstrap binds this socket before it spawns
/// the companion, and the companion connects to it and writes [`GateMessage`] lines: first the
/// sessions-manager's verdict, and for a replaced task, later the exit code. Without the variable
/// the companion doesn't gate the task, and the application starts as it would without
/// admission.
pub const ADMISSION_SOCKET_ENV: &str = "MIRRORD_REMOTE_ADMISSION_SOCKET";

/// Environment variable carrying the PID of the application process the bootstrap runs in, set
/// by the bootstrap for the workload companion.
///
/// The companion reads the application's command line from `/proc/<pid>/cmdline`, so a developer
/// can start a local process with the task's own arguments.
pub const APP_PID_ENV: &str = "MIRRORD_REMOTE_APP_PID";

/// The exit code of a replaced task whose developer session was lost before it reported the
/// local process's exit code.
///
/// Above the codes of processes killed by a signal (`128 + signal`, at most 192 on Linux), which a
/// replaced task also ends with, so a job definition can tell a lost session apart, for example
/// to retry the job with `evaluateOnExit` in AWS Batch.
pub const LOST_SESSION_EXIT_CODE: i32 = 214;

/// One line the workload companion writes to the bootstrap over [`ADMISSION_SOCKET_ENV`].
///
/// The line format is plain text, one message per line: `continue`, `replace` or
/// `exit <code>`. A reader treats a line it can't parse as no verdict at all, which before a
/// verdict means the application starts (failing open), and after `replace` means the task ends
/// with [`LOST_SESSION_EXIT_CODE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateMessage {
    /// The task runs its own code. The bootstrap loads the remote layer and lets the application
    /// start, and the companion sends nothing more.
    Continue,
    /// A developer's local process runs in place of the task: the application never starts, and
    /// the bootstrap waits without a deadline for [`Self::Exit`].
    Replace,
    /// Ends a replaced task with this exit code, the local process's or
    /// [`LOST_SESSION_EXIT_CODE`].
    Exit(i32),
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

    /// Reads one line, with or without its line ending. [`None`] for a line that isn't a
    /// message.
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

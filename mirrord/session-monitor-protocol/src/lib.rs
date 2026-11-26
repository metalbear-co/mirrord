use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

/// File extension used for the per-session sentinel file in `~/.mirrord/sessions/`. On unix
/// this file IS the bound Unix domain socket; on windows it is an empty marker file pointing
/// at the named pipe whose name is given by `pipe_name_for_session` (windows-only). Producer
/// (intproxy) and consumer (session-monitor-client) both use this constant so filesystem-
/// watcher-based discovery agrees on what to look for.
pub const SESSION_SENTINEL_EXTENSION: &str = if cfg!(windows) { "pipe" } else { "sock" };

/// Address of the named pipe a session's HTTP API binds to on windows. The producer creates
/// this pipe; the consumer connects to it.
#[cfg(windows)]
pub fn pipe_name_for_session(session_id: &str) -> String {
    format!(r"\\.\pipe\mirrord-session-{session_id}")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    #[serde(default)]
    pub parent_pid: Option<u32>,
    pub process_name: String,
    #[serde(default)]
    pub cmdline: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortSubscription {
    pub port: u16,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_count: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    #[serde(default)]
    pub key: Option<String>,
    pub target: String,
    #[serde(default)]
    pub namespace: Option<String>,
    /// Kube context this session runs against, for display in the `mirrord ui`. Lets the UI label
    /// local sessions with their cluster even though they're shown regardless of the selected
    /// context/namespace. `None` when it couldn't be resolved from the kubeconfig.
    #[serde(default)]
    pub context: Option<String>,
    /// Kubeconfig this session reads: the `kubeconfig` option, else the `KUBECONFIG` variable it
    /// started with, which can list several files. `None` means the default `~/.kube/config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kubeconfig: Option<String>,
    pub started_at: String,
    pub mirrord_version: String,
    pub is_operator: bool,
    #[serde(default)]
    pub processes: Vec<ProcessInfo>,
    #[serde(default)]
    pub port_subscriptions: Vec<PortSubscription>,
    pub config: serde_json::Value,
}

/// Path of the captured requests routes in a session's HTTP API.
pub const REPLAY_REQUESTS_ROUTE: &str = "/replay/requests";

/// The part of an HTTP body that the session keeps.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapturedBody {
    /// The kept bytes as text. Bytes that aren't valid UTF-8 are replaced.
    pub text: String,

    /// Size of the body in bytes. For a request body that didn't arrive whole, and for the body
    /// of a response to a replay with `truncated` set, this is the size of the part that the
    /// session read.
    pub size: u64,

    /// Whether the session read more of the body than `text` holds, because the body is larger
    /// than the session keeps, or because a response to a replay didn't end in time. A request
    /// body that didn't arrive whole is marked by `not_replayable` instead.
    pub truncated: bool,
}

/// How the local application answered a captured request or a replay of it, without the body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapturedResponse {
    pub status: u16,

    /// Response headers. Values of headers that usually carry secrets are redacted.
    pub headers: Vec<(String, String)>,

    /// Milliseconds between sending the request and receiving the response headers.
    pub duration_ms: u64,
}

/// One replay of a captured request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayRun {
    /// Position of this replay among the replays of its request, starting at 1.
    pub number: u32,

    /// When the replay was sent, in milliseconds since the Unix epoch.
    pub at_ms: u64,

    pub response: CapturedResponse,

    /// The response body.
    pub body: CapturedBody,
}

/// Why a captured request can't be replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotReplayable {
    /// The request upgraded the connection, for example to a WebSocket.
    Upgraded,

    /// The body is larger than the session keeps.
    BodyTooLarge,

    /// The session doesn't have the whole body: it is still arriving, the client stopped sending
    /// it, or the local application answered before all of it arrived.
    BodyIncomplete,
}

/// Completes the sentence "the request can't be replayed, because".
impl fmt::Display for NotReplayable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Upgraded => "it upgraded the connection",
            Self::BodyTooLarge => "its body is larger than the session keeps",
            Self::BodyIncomplete => "the session doesn't have its whole body",
        })
    }
}

/// An HTTP request that the session received for the local application, without its headers,
/// body and replays.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapturedRequestSummary {
    /// Identifies the request within its session.
    pub id: u64,

    /// When the session received the request, in milliseconds since the Unix epoch.
    pub captured_at_ms: u64,

    /// How the request reached the session: `steal` or `mirror`.
    pub mode: String,

    /// The remote port that received the request.
    pub port: u16,

    pub method: String,

    /// Path and query of the request.
    pub path: String,

    /// Status of the response to the original request, not to a replay. `None` while the local
    /// application hasn't answered it, and when it never did.
    #[serde(default)]
    pub status: Option<u16>,

    /// Milliseconds the local application took to answer the original request. `None` when
    /// `status` is.
    #[serde(default)]
    pub duration_ms: Option<u64>,

    /// Size of the request body in bytes, or of the part that arrived when `not_replayable` is
    /// `body_incomplete`.
    pub body_size: u64,

    /// Why the request can't be replayed. `None` when it can.
    #[serde(default)]
    pub not_replayable: Option<NotReplayable>,

    /// How many times the request was replayed.
    pub replay_count: u32,
}

/// An HTTP request that the session received for the local application.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapturedRequest {
    #[serde(flatten)]
    pub summary: CapturedRequestSummary,

    /// HTTP version of the request, for example `HTTP/1.1`.
    pub version: String,

    /// Request headers. Values of headers that usually carry secrets are redacted.
    pub headers: Vec<(String, String)>,

    pub body: CapturedBody,

    /// The response to the original request, not to a replay. `None` while the local
    /// application hasn't answered it, and when it never did.
    #[serde(default)]
    pub response: Option<CapturedResponse>,

    /// The latest replays of the request, oldest first.
    #[serde(default)]
    pub replays: Vec<ReplayRun>,
}

/// Changes to make to a captured request before replaying it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReplayOptions {
    /// Headers to set on the replayed request, by name. A value replaces every captured value of
    /// that header, and `None` removes the header.
    #[serde(default)]
    pub headers: BTreeMap<String, Option<String>>,
}

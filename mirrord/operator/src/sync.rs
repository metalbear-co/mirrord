//! The wire protocol of preview code sync, which pushes the CLI's local tree into a preview pod.
//!
//! The CLI and the sync sidecar in the preview pod never talk directly. The CLI opens the
//! `previews/{name}/sync` websocket on the operator, the sidecar dials the operator's gateway, and
//! the operator relays [`SyncFrame`]s between the two connections. Each websocket message holds one
//! frame, and on the gateway connection each frame is prefixed with its length as a big-endian
//! `u32`.
//!
//! This module needs no feature of the crate, so the static sidecar can use it without pulling in
//! `kube`.

use bincode::{Decode, Encode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum_macros::{Display, IntoStaticStr};

use self::rules::IgnoreSnapshot;

pub mod rules;

#[cfg(test)]
mod tests;

/// The version of the protocol, which the sidecar sends after [`SYNC_HELLO`].
pub const SYNC_PROTOCOL_VERSION: u8 = 1;

/// The first two bytes that a sync sidecar sends on a gateway connection.
///
/// The DB sidecar sends the length of a branch name there, and no branch name is this long, so the
/// gateway tells the two kinds of sidecar apart by these bytes.
pub const SYNC_HELLO: u16 = 0xFFFF;

/// The largest encoded frame that either side accepts.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The largest chunk of a tar stream that the CLI puts in one [`SyncFrame::Data`].
pub const MAX_DATA_CHUNK_BYTES: usize = 256 * 1024;

/// One message of the sync protocol.
///
/// `bincode` isn't self-describing, so once released, the enum may only grow: a new variant goes
/// at the end, and a changed message becomes a new variant. The same holds for every type that a
/// frame carries.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq, IntoStaticStr)]
pub enum SyncFrame {
    /// CLI to sidecar: starts a push of the entries that `entries` plans.
    Begin {
        push_id: u64,
        mode: PushMode,
        entries: Vec<EntryPlan>,
    },

    /// CLI to sidecar: the next chunk of the gzip-compressed tar stream of entry `index`.
    Data { index: u32, chunk: Vec<u8> },

    /// CLI to sidecar: every chunk of entry `index` is sent. In an incremental push, `deleted`
    /// lists the paths of the entry that the push deletes, relative to the entry, as the bytes of
    /// `/`-separated components, because a file name needn't be UTF-8. A full push lists none,
    /// because it deletes every path that it doesn't carry and that isn't ignored.
    EntryDone { index: u32, deleted: Vec<Vec<u8>> },

    /// CLI to sidecar: every entry of the push is done, so the sidecar applies it.
    Commit { push_id: u64 },

    /// Sidecar to CLI: the push is on the volume, as generation `generation`.
    Applied {
        push_id: u64,
        generation: u64,
        entries: Vec<EntryCounts>,
    },

    /// Sidecar to CLI: the push failed, and the volume holds what it held before.
    Rejected { push_id: u64, reason: RejectReason },

    /// Sidecar to operator: the sidecar's state, sent on connect and whenever it changes.
    SidecarState(SidecarState),

    /// Operator to CLI: the sidecar's state while the CLI waits for [`SyncFrame::Paired`].
    Status(SidecarState),

    /// Operator to CLI: the sidecar is ready, so the CLI can push. `generation` is what the
    /// volume holds, and `protocol_version` is the sidecar's version of the protocol, which can
    /// be older than the CLI's until the preview pod restarts after an operator upgrade.
    Paired {
        generation: Option<u64>,
        protocol_version: u8,
    },

    /// Operator to CLI: the sync can't go on, and the operator closes the connection.
    Error { message: String },

    /// Sidecar to operator, and operator to CLI: what the supervisor of the app container
    /// reported about the install and the app, sent on connect and whenever it changes.
    Supervisor(SupervisorReport),

    /// Operator to CLI: the CLI can't push for now, for the reason in `reason`. The connection
    /// stays open, except after [`UnpairReason::Replaced`], and the operator sends
    /// [`SyncFrame::Paired`] once the CLI can push again.
    Unpaired { reason: UnpairReason },

    /// CLI to operator: wakes the preview if it is idle, as traffic to it would.
    WakeRequest,
}

/// Why the operator unpaired a CLI. Once released, it may only grow, as [`SyncFrame`] says.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Display)]
pub enum UnpairReason {
    /// The preview scaled to zero after its idle timeout, and its pod is gone until a wake.
    #[strum(to_string = "the preview is idle")]
    Idle,

    /// A `mirrord exec` session replaces the preview for now.
    #[strum(to_string = "a `mirrord exec` session replaces the preview for now")]
    Paused,

    /// The preview pod's sync sidecar disconnected, for example because its pod was replaced.
    #[strum(to_string = "the preview pod's sync sidecar disconnected")]
    SidecarGone,

    /// Another CLI took over the sync of the preview, and the operator closes this connection.
    #[strum(
        to_string = "another `mirrord preview sync` took over the code sync of the preview \
                     environment"
    )]
    Replaced,
}

/// What a push replaces in each of its entries. Once released, it may only grow, as
/// [`SyncFrame`] says.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushMode {
    /// Each entry of the push becomes a mirror of the CLI's tree: the sidecar deletes every path
    /// that the push doesn't carry and that the entry's rules don't ignore.
    Full,

    /// The push only writes the paths that it carries and deletes the paths that
    /// [`SyncFrame::EntryDone`] lists. It applies only on top of generation `base_generation`,
    /// and the sidecar rejects it with [`RejectReason::StaleBase`] otherwise.
    Incremental { base_generation: u64 },
}

/// What a push carries for one entry, declared in [`SyncFrame::Begin`] before the data, so that
/// the sidecar can refuse a push that is too large before it writes anything.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct EntryPlan {
    /// The index of the entry in the session's `spec.sync`.
    pub index: u32,

    /// The number of files and symlinks in the entry's tar stream.
    pub files: u64,

    /// The number of directories in the entry's tar stream. Each costs a tar header, which the
    /// sidecar counts against the stream's size.
    pub directories: u64,

    /// The total size of the files in the entry's tar stream, unpacked.
    pub bytes: u64,

    /// The rules that decide which paths of the entry the push leaves out, and which ones a full
    /// push keeps on the volume.
    pub rules: IgnoreSnapshot,
}

/// What a push changed in one entry.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryCounts {
    /// The index of the entry in the session's `spec.sync`.
    pub index: u32,

    /// The files and symlinks that the push wrote, because they were new or changed.
    pub written: u64,

    /// The files and symlinks of the push that the entry already held as they were.
    pub unchanged: u64,

    /// The paths that the push deleted.
    pub deleted: u64,
}

/// Why the sidecar rejected a push. Once released, it may only grow, as [`SyncFrame`] says.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq, Display)]
pub enum RejectReason {
    /// An incremental push named a base generation that the volume no longer holds. The CLI
    /// should send a full push instead.
    #[strum(to_string = "the push was based on a generation that the volume no longer holds")]
    StaleBase,

    /// The sidecar isn't ready for a push yet: it waits for the code volume, or copies the
    /// image's files to it.
    #[strum(to_string = "the preview pod's sync sidecar isn't ready for a push yet")]
    NotReady,

    /// The push breaks the protocol, for example with a path that leaves its entry.
    #[strum(to_string = "the push is invalid: {0}")]
    Invalid(String),

    /// Applying the push at this path would destroy paths that the rules ignore, such as a file
    /// that replaces an ignored directory, or a directory with ignored paths in it.
    #[strum(
        to_string = "`{0}` would replace a directory that the push's rules ignore, or that holds \
                     ignored paths, which a push never deletes; ignore `{0}`, or remove that \
                     directory in the preview pod"
    )]
    Conflict(String),

    /// The code volume ran out of space.
    #[strum(to_string = "the code volume is full; raise `operator.preview.codeSync.storage.size`")]
    Space,

    /// The push is larger than the sidecar accepts.
    #[strum(to_string = "the push is too large: {0}")]
    Limit(String),

    /// The sidecar failed to read or write the volume.
    #[strum(to_string = "the sync sidecar failed to read or write the code volume: {0}")]
    Io(String),
}

/// What the sync sidecar is doing. Once released, it may only grow, as [`SyncFrame`] says. Its
/// `Display` is the wording of `mirrord preview sync`.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq, Display)]
pub enum SidecarState {
    /// No sidecar of the preview is connected to the operator.
    #[strum(to_string = "waiting for the preview pod")]
    Disconnected,

    /// The sidecar waits for the sidecar and the supervisor of an earlier pod to release the
    /// code volume.
    #[strum(to_string = "waiting for the previous pod to release the code volume")]
    WaitingForVolume,

    /// The sidecar copies the image's files at each `remote` and `install.persist` path to the
    /// code volume.
    #[strum(to_string = "copying the image's files to the code volume")]
    Seeding,

    /// The volume has no push yet.
    #[strum(to_string = "waiting for the first push")]
    AwaitingFirstPush,

    /// The volume holds the push of generation `generation`.
    #[strum(to_string = "holds generation {generation}")]
    Synced { generation: u64 },

    /// The sidecar can't go on, for the reason in `message`, such as a volume that its user can't
    /// write to.
    #[strum(to_string = "failed: {message}")]
    Failed { message: String },
}

/// What the supervisor, the first process of the app container, reports about the install
/// command and the app. The supervisor sends it to the sidecar on their Unix socket, as JSON, and
/// the sidecar sends it on as [`SyncFrame::Supervisor`]. Once released, it may only grow, as
/// [`SyncFrame`] says.
#[derive(Encode, Decode, Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SupervisorReport {
    pub install: InstallReport,
    pub app: AppReport,

    /// The generation of the last push that the supervisor acted on: it ran the install that the
    /// push needed, and the app that it started or restarted for the push, as the session's
    /// settings say, has started. `None` before it acted on a push. Once this reaches the
    /// generation of a CLI's push, the supervisor is done with that push, and `install` says
    /// whether its install failed.
    pub handled: Option<u64>,
}

/// The last run of `install.command`. The error sits beside the state rather than in
/// [`InstallState::Failed`], so that the state stays one word in the session's status, as
/// `kubectl` shows it.
#[derive(
    Encode, Decode, Serialize, Deserialize, JsonSchema, Debug, Clone, Default, PartialEq, Eq,
)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub state: InstallState,

    /// Why the last install failed, with the end of its output. `None` unless `state` is
    /// [`InstallState::Failed`].
    pub error: Option<String>,
}

/// Where the install stands. Once released, it may only grow, as [`SyncFrame`] says. Its
/// `Display` is the wording of `mirrord preview sync` and `mirrord preview status`.
#[derive(
    Encode,
    Decode,
    Serialize,
    Deserialize,
    JsonSchema,
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Display,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum InstallState {
    /// No install ran in this container yet: the supervisor waits for the sidecar's first
    /// answer, or the volume's install is up to date, or the session has no `install`.
    #[default]
    #[strum(to_string = "not run")]
    NotRun,
    Running,
    Succeeded,

    /// The last install failed. The app keeps running on the earlier install, or doesn't start
    /// when no install ever succeeded, and the next push runs the install again.
    Failed,

    /// A state that a newer supervisor or operator reports, which this version doesn't know.
    #[serde(other)]
    Unknown,
}

/// The app, the process of `run.command`.
#[derive(
    Encode, Decode, Serialize, Deserialize, JsonSchema, Debug, Clone, Default, PartialEq, Eq,
)]
#[serde(rename_all = "camelCase")]
pub struct AppReport {
    pub state: AppState,

    /// How many times the app exited on its own, or couldn't start, and the supervisor started
    /// it again.
    pub restarts: u32,

    /// The exit code of the app's last run, or 128 plus the number of the signal that ended it,
    /// as a shell reports it. A program that couldn't start counts as 127, as a shell reports a
    /// command that isn't found.
    pub last_exit_code: Option<i32>,
}

/// Where the app stands. Once released, it may only grow, as [`SyncFrame`] says. Its `Display`
/// is the wording of `mirrord preview sync` and `mirrord preview status`.
#[derive(
    Encode,
    Decode,
    Serialize,
    Deserialize,
    JsonSchema,
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Display,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum AppState {
    /// The app hasn't started yet: the supervisor waits for the sidecar's first answer, or the
    /// first install is running or failed.
    #[default]
    #[strum(to_string = "not started")]
    NotStarted,
    Running,

    /// The app starts again: it stops to start on a new push or install, or it exited on its own
    /// and the supervisor starts it again after a backoff.
    Restarting,

    /// The container stops, and the app is gone.
    Exited,

    /// A state that a newer supervisor or operator reports, which this version doesn't know.
    #[serde(other)]
    Unknown,
}

impl SidecarState {
    /// Whether the sidecar accepts a push in this state.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::AwaitingFirstPush | Self::Synced { .. })
    }
}

impl SyncFrame {
    /// The name of the frame's variant, for a message about the frame. A `Data` frame's `Debug`
    /// would hold its whole chunk.
    pub fn name(&self) -> &'static str {
        self.into()
    }

    /// Encodes the frame with the `bincode` configuration that both sides use.
    pub fn encode(&self) -> Vec<u8> {
        bincode::encode_to_vec(self, bincode::config::standard())
            .expect("a sync frame always encodes")
    }

    /// Decodes a frame that [`SyncFrame::encode`] produced.
    ///
    /// `bincode` allocates a vector or a string at the length that the frame claims before it
    /// reads the bytes, so the decoder refuses lengths above [`MAX_FRAME_BYTES`]. Without the
    /// limit, a frame of a few bytes that claims a chunk of terabytes would abort the process.
    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        let config = bincode::config::standard().with_limit::<MAX_FRAME_BYTES>();
        let (frame, _) = bincode::decode_from_slice(bytes, config)?;
        Ok(frame)
    }
}

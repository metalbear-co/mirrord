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
/// `bincode` isn't self-describing, so the enum may only grow: a new variant goes at the end, and
/// a changed message becomes a new variant.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub enum SyncFrame {
    /// CLI to sidecar: starts a push, which replaces the content of every entry in `entries`.
    Begin { push_id: u64, entries: Vec<u32> },

    /// CLI to sidecar: the next chunk of the gzip-compressed tar stream of entry `index`.
    Data { index: u32, chunk: Vec<u8> },

    /// CLI to sidecar: every chunk of the push is sent, so the sidecar applies it.
    Commit { push_id: u64 },

    /// Sidecar to CLI: the push is on the volume, as generation `generation`.
    Applied { push_id: u64, generation: u64 },

    /// Sidecar to CLI: the push failed, and the volume holds what it held before.
    Rejected { push_id: u64, reason: String },

    /// Sidecar to operator: the sidecar's state, sent on connect and whenever it changes.
    SidecarState(SidecarState),

    /// Operator to CLI: the sidecar's state while the CLI waits for [`SyncFrame::Paired`].
    Status(SidecarState),

    /// Operator to CLI: the sidecar is ready, so the CLI can push.
    Paired { generation: Option<u64> },

    /// Operator to CLI: the sync can't go on, and the operator closes the connection.
    Error { message: String },
}

/// What the sync sidecar is doing.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarState {
    /// No sidecar of the preview is connected to the operator.
    Disconnected,

    /// The volume has no push yet.
    AwaitingFirstPush,

    /// The volume holds the push of generation `generation`.
    Synced { generation: u64 },
}

impl SidecarState {
    /// Whether the sidecar accepts a push in this state.
    pub fn is_ready(self) -> bool {
        matches!(self, Self::AwaitingFirstPush | Self::Synced { .. })
    }
}

impl SyncFrame {
    /// Encodes the frame with the `bincode` configuration that both sides use.
    pub fn encode(&self) -> Vec<u8> {
        bincode::encode_to_vec(self, bincode::config::standard())
            .expect("a sync frame always encodes")
    }

    /// Decodes a frame that [`SyncFrame::encode`] produced.
    pub fn decode(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        let (frame, _) = bincode::decode_from_slice(bytes, bincode::config::standard())?;
        Ok(frame)
    }
}

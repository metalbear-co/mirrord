//! What the layer tells the internal proxy about the time the application spent waiting on
//! mirrord.
//!
//! The layer keeps a running tally per [`CallKind`] and sends all of it as a
//! [`CallTimingsSnapshot`] from time to time. Every snapshot carries the totals since the process
//! started, so the proxy only ever needs the latest one per layer.

use bincode::{Decode, Encode};
use mirrord_protocol::FileRequest;
use strum::VariantArray;
use strum_macros::{IntoStaticStr, VariantArray};

use crate::{IncomingRequest, LayerToProxyMessage, OutgoingRequest};

/// The kinds of calls the application waits on mirrord for, as shown to the user.
///
/// Only requests that are answered from the cluster have a kind. Calls the layer passes straight
/// to the operating system never become a request, and requests the internal proxy answers from
/// its own state (connection metadata) are left out, so neither waters the numbers down.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Hash, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum CallKind {
    NameLookup,
    OutgoingConnect,
    PortSubscribe,
    EnvFetch,
    FileOpen,
    FileRead,
    FileWrite,
    FileSeek,
    FileStat,
    FileAccess,
    DirectoryListing,
    ReadLink,
    FileChange,
}

impl CallKind {
    /// Every kind, in the order they are declared. A kind's position here is its `as usize`
    /// value, which the layer uses as an index into its counters.
    pub const ALL: &'static [Self] = <Self as VariantArray>::VARIANTS;

    /// The kind of the call behind `message`, or [`None`] when the message is not a call the
    /// application waits on the cluster for.
    pub fn of(message: &LayerToProxyMessage) -> Option<Self> {
        match message {
            LayerToProxyMessage::GetAddrInfo(_) => Some(Self::NameLookup),
            LayerToProxyMessage::Outgoing(OutgoingRequest::Connect(_)) => {
                Some(Self::OutgoingConnect)
            }
            LayerToProxyMessage::Incoming(IncomingRequest::PortSubscribe(_)) => {
                Some(Self::PortSubscribe)
            }
            LayerToProxyMessage::GetEnv(_) => Some(Self::EnvFetch),
            LayerToProxyMessage::File(request) => Self::of_file_request(request),
            LayerToProxyMessage::NewSession(_)
            | LayerToProxyMessage::Outgoing(_)
            | LayerToProxyMessage::Incoming(_)
            | LayerToProxyMessage::CallTimings(_) => None,
        }
    }

    fn of_file_request(request: &FileRequest) -> Option<Self> {
        match request {
            FileRequest::Open(_) | FileRequest::OpenRelative(_) => Some(Self::FileOpen),
            FileRequest::Read(_) | FileRequest::ReadLimited(_) => Some(Self::FileRead),
            FileRequest::Write(_) | FileRequest::WriteLimited(_) => Some(Self::FileWrite),
            FileRequest::Seek(_) => Some(Self::FileSeek),
            FileRequest::Xstat(_)
            | FileRequest::XstatFs(_)
            | FileRequest::XstatFsV2(_)
            | FileRequest::StatFs(_)
            | FileRequest::StatFsV2(_) => Some(Self::FileStat),
            FileRequest::Access(_) => Some(Self::FileAccess),
            FileRequest::FdOpenDir(_)
            | FileRequest::ReadDir(_)
            | FileRequest::ReadDirBatch(_)
            | FileRequest::GetDEnts64(_) => Some(Self::DirectoryListing),
            FileRequest::ReadLink(_) => Some(Self::ReadLink),
            FileRequest::MakeDir(_)
            | FileRequest::MakeDirAt(_)
            | FileRequest::RemoveDir(_)
            | FileRequest::Unlink(_)
            | FileRequest::UnlinkAt(_)
            | FileRequest::Rename(_)
            | FileRequest::Ftruncate(_)
            | FileRequest::Futimens(_)
            | FileRequest::Fchown(_)
            | FileRequest::Fchmod(_) => Some(Self::FileChange),
            // The layer waits for no answer to these.
            FileRequest::Close(_) | FileRequest::CloseDir(_) => None,
        }
    }
}

/// The tally of one [`CallKind`] in one process, since that process started.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindTimings {
    pub kind: CallKind,
    /// How many calls of this kind got an answer or an error.
    pub calls: u64,
    /// Time the application spent waiting for all of them.
    pub total_nanos: u64,
    /// The longest wait for a single call.
    pub slowest_nanos: u64,
}

/// How many of its slowest calls a process keeps and sends, and how many the session's summary
/// lists.
///
/// Eight is enough to see a pattern (the same host three times, one directory) and short enough
/// to read at a glance. With [`MAX_SUBJECT_BYTES`] it also keeps a snapshot under 3 KB.
pub const MAX_SLOW_CALLS: usize = 8;

/// The longest subject kept for a slow call, in bytes. A path can be 4 KB long, and eight of
/// those would turn a small periodic message into a large one. A longer subject is cut and ends
/// in `...` so the reader sees it was.
pub const MAX_SUBJECT_BYTES: usize = 256;

/// One of the slowest calls a process made, with what it was for.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct SlowCall {
    pub kind: CallKind,
    /// What the call was about: the host of a name lookup, the path of a file operation, the
    /// address of an outgoing connection. [`None`] for calls on an already open file and the
    /// kinds that have no single subject.
    pub subject: Option<String>,
    /// How long the application waited for it.
    pub nanos: u64,
}

impl SlowCall {
    /// What the call behind `message` was about, cut to [`MAX_SUBJECT_BYTES`].
    pub fn subject_of(message: &LayerToProxyMessage) -> Option<String> {
        let subject = match message {
            LayerToProxyMessage::GetAddrInfo(request) => request.node.clone(),
            LayerToProxyMessage::Outgoing(OutgoingRequest::Connect(request)) => {
                match &request.metadata.hostname {
                    Some(hostname) => format!("{hostname} ({})", request.remote_address),
                    None => request.remote_address.to_string(),
                }
            }
            LayerToProxyMessage::File(request) => {
                let path = match request {
                    FileRequest::Open(request) => &request.path,
                    FileRequest::OpenRelative(request) => &request.path,
                    FileRequest::Access(request) => &request.pathname,
                    FileRequest::Xstat(request) => request.path.as_ref()?,
                    FileRequest::ReadLink(request) => &request.path,
                    FileRequest::StatFs(request) => &request.path,
                    FileRequest::StatFsV2(request) => &request.path,
                    FileRequest::MakeDir(request) => &request.pathname,
                    FileRequest::MakeDirAt(request) => &request.pathname,
                    FileRequest::RemoveDir(request) => &request.pathname,
                    FileRequest::Unlink(request) => &request.pathname,
                    FileRequest::UnlinkAt(request) => &request.pathname,
                    FileRequest::Rename(request) => &request.old_path,
                    _ => return None,
                };
                path.to_string_lossy().into_owned()
            }
            _ => return None,
        };

        Some(cut_to_budget(subject))
    }
}

fn cut_to_budget(mut subject: String) -> String {
    const CUT_MARK: &str = "...";

    if subject.len() <= MAX_SUBJECT_BYTES {
        return subject;
    }

    let mut end = MAX_SUBJECT_BYTES.saturating_sub(CUT_MARK.len());
    while !subject.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    subject.truncate(end);
    subject.push_str(CUT_MARK);
    subject
}

/// Every [`CallKind`] a process has made at least one call of, with totals since it started, and
/// the slowest single calls among them.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq, Default)]
pub struct CallTimingsSnapshot {
    pub kinds: Vec<KindTimings>,
    /// At most [`MAX_SLOW_CALLS`], in no particular order.
    pub slowest: Vec<SlowCall>,
}

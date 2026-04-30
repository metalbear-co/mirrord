use strum::VariantArray;
use strum_macros::VariantArray;

use crate::client::enum_map::{EnumKey, EnumMap};

/// Kind of a [DaemonMessage](`mirrord_protocol::codec::DaemonMessage`) responses queue.
///
/// Used with [SimpleRequest](`crate::client::simple_request::SimpleRequest`) to match responses to
/// previously sent requests.
///
/// Within each queue, the server is expected to preserve the order of responses.
#[derive(Debug, Clone, Copy, VariantArray)]
#[repr(u8)]
pub enum QueueKind {
    EnvVars,
    Dns,
    ReverseDns,
    Files,
}

impl EnumKey for QueueKind {
    fn into_index(self) -> usize {
        self as usize
    }
}

pub type QueueKindMap<T> = EnumMap<QueueKind, T, { QueueKind::VARIANTS.len() }>;

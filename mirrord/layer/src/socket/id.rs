//! The types in this module should **NOT** be constructed directly.

use std::sync::atomic::AtomicU64;

/// Holds the latest `u64` to be used as a new [`SocketId`].
static SOCKET_ID_ALLOCATOR: AtomicU64 = AtomicU64::new(0);

/// Better way of identifying a socket than just relying on its `fd`.
///
/// ## Details
///
/// Due to how we handle [`ops::dup`], if we were to rely solely on `fd`s to identify a socket, then
/// we can miss changes that should happen on the _original_ `fd` (but were triggered on the
/// _dupped_ `fd`).
///
/// This is mostly to help the [`ConnectionQueue`] tracking the correct socket for [`ops::accept`].
#[derive(Debug, PartialOrd, PartialEq, Ord, Eq, Clone, Copy, Hash)]
pub(crate) struct SocketId(u64);

impl Default for SocketId {
    /// Increments [`SOCKET_ID_ALLOCATOR`] and uses the latest value as an id for `Self`.
    fn default() -> Self {
        Self(SOCKET_ID_ALLOCATOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

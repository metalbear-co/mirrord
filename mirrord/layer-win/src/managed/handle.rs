//! Type-level distinctions between handle kinds.
//!
//! Each subsystem defines its own newtype around
//! [`winapi::shared::ntdef::HANDLE`] that implements
//! [`ManagedHandleKey`]. Kinds whose values are minted from the registry's
//! own monotonic counter additionally implement [`CounterAllocated`]; file
//! handles do this so the OS-side handle space and ours stay disjoint.
//!
//! The trait bounds are exactly what a
//! [`ManagedRegistry`](super::registry::ManagedRegistry) needs: copy/eq/hash
//! for HashMap keying, `Send + Sync` for the cross-thread static, and
//! `Borrow<HANDLE>` so callers can `map.get(&raw_handle_from_syscall)`
//! without first wrapping the raw handle into the newtype.

use std::{borrow::Borrow, hash::Hash};

/// Marker trait for handle keys stored in a
/// [`ManagedRegistry`](super::registry::ManagedRegistry). Every kind of
/// managed handle implements this; counter-allocated kinds additionally
/// implement [`CounterAllocated`] below.
pub(crate) trait ManagedHandleKey:
    Copy + Eq + Hash + Send + Sync + Borrow<winapi::shared::ntdef::HANDLE> + 'static
{
}

/// Extension for keys whose values are minted by a monotonic counter
/// inside the registry. File handles use this -- the OS would otherwise
/// hand us back a real handle we'd have to swap out, so we just allocate
/// a synthetic value from a disjoint numeric range and intercept ops on
/// it.
pub(crate) trait CounterAllocated: ManagedHandleKey {
    /// The first numeric value the registry's counter will hand out. Pick
    /// a value comfortably outside the OS handle space (the kernel hands
    /// out small even integers).
    const FIRST: usize;

    /// Wrap a raw counter tick into the typed handle. Always called with
    /// values >= [`Self::FIRST`].
    fn from_raw(raw: usize) -> Self;
}

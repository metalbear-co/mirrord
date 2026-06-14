//! Generic plumbing for "managed handles": handles whose values the layer
//! hands out itself (so we know any operation on them is one we should
//! intercept) instead of letting the OS allocate them.
//!
//! Each subsystem wires up its own concrete [`ManagedHandleKey`](handle::ManagedHandleKey) newtype
//! plus a [`ManagedRegistry`] storing whatever per-handle context it cares about.
//!
//! The registry is the lifted form of what `hooks/files/managed_handle.rs` used to spell out by
//! hand (`Lazy<RwLock<HashMap<MirrordHandle, Arc<RwLock<HandleContext>>>>>`). Today it is backed by
//! a lock-sharded [`ShardedMap`](sharded_map::ShardedMap), so concurrent inserts, lookups, and
//! removes across unrelated handles don't serialize on one lock. That single lock previously
//! contended badly enough under load to drop operations.
//!
//! The distinct [`ManagedHandleKey`](handle::ManagedHandleKey) newtypes keep the numeric value
//! ranges of the different domains disjoint. The kind of a leaked handle is then obvious from its
//! value (file handles live in `0x5000_xxxx`). And a typo that passes one kind of handle where
//! another was expected is caught at the type-system level.

pub(crate) mod handle;
pub(crate) mod registry;
pub(crate) mod sharded_map;

pub(crate) use registry::ManagedRegistry;

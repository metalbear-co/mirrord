//! Generic plumbing for "managed handles": handles whose values the layer
//! hands out itself (so we know any operation on them is one we should
//! intercept) instead of letting the OS allocate them.
//!
//! Each subsystem that needs managed handles wires up its own concrete
//! [`ManagedHandleKey`] newtype plus a [`ManagedRegistry`] storing whatever
//! per-handle context it cares about. The registry is the lifted form of
//! what `hooks/files/managed_handle.rs` used to spell out by hand
//! (`Lazy<RwLock<HashMap<MirrordHandle, Arc<RwLock<HandleContext>>>>>`).
//!
//! Kind tagging via [`HandleKind`] keeps the numeric value ranges of the
//! different domains disjoint -- the kind of a leaked handle is obvious
//! from its numeric value (file handles live in `0x5000_xxxx`), and a
//! typo that passes a handle of one kind where another was expected is
//! caught at the type system level via the distinct newtypes.

pub(crate) mod handle;
pub(crate) mod registry;

pub(crate) use registry::ManagedRegistry;

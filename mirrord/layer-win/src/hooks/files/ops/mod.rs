//! Per-operation handlers. Each FS hook in the
//! [FS-hook dispatcher](super) is a thin
//! `unsafe extern "system" fn` that delegates to a `pub(super)` function
//! here. Splitting by operation keeps each module under ~250 lines and
//! puts everything that defines "what mirrord does when an
//! `NtSomething` arrives" in one searchable place.
//!
//! Module map:
//!
//! - [`open`] — `nt_create_file_hook` (filter, map, open remote, register)
//! - [`read`] — `nt_read_file_hook` (sync/async paths) + worker bodies
//! - [`set_info`] — `nt_set_information_file_hook` (per-class dispatch)
//! - [`query_info`] — `nt_query_information_file_hook` and `nt_query_volume_information_file_hook`
//! - [`close`] — `nt_close_hook` (CloseFileRequest + IOCP unbind)
//! - [`cancel`] — `nt_cancel_io_file_hook` (acknowledge invariant)
//! - [`wait`] — `nt_wait_for_single_object_hook` (fake success for managed)
//! - [`stubs`] — warn-and-passthrough hooks for unimplemented operations (volume info set, quota,
//!   attributes query, delete, ioctl, lock, unlock, write). Step 3 of the reorg will macroize
//!   these.

pub(super) mod cancel;
pub(super) mod close;
pub(super) mod open;
pub(super) mod query_info;
pub(super) mod read;
pub(super) mod set_info;
pub(super) mod stubs;
pub(super) mod wait;

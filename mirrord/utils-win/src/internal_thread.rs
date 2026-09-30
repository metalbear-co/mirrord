//! Marks mirrord's own threads so hooked APIs pass their traffic through.
//!
//! The layer enables every hook family synchronously inside `DllMain`, before any user code
//! can run. Mirrord's own threads still have to reach the internal proxy and the local disk
//! afterwards. Without this marker their `socket`/`connect`/file calls are intercepted and
//! routed back through mirrord, which is wrong in three places:
//!
//! - the startup worker, whose traffic *is* the proxy connection being established,
//! - the `task_pool` workers, which run the agent round-trips for async reads and DNS,
//! - the crash handler, which must write its report to the local disk, allocation-free, on a
//!   faulting thread.
//!
//! It lives in `utils-win` rather than `layer-win` because the crash handler is here and
//! `utils-win` cannot depend on `layer-win`. `layer-win` re-exports it as
//! `crate::hooks::internal_thread`, which is the path the `internal_bypass` macro expands to.
//!
//! # Which hooks are not annotated, and why
//!
//! `#[internal_bypass]` answers "who is calling?". A hook that instead dispatches on *what it
//! was given* must not carry it, because an internal thread can still hold a synthetic managed
//! handle. Bypassing would hand a `0x5000_xxxx` value straight to the kernel, which answers
//! `STATUS_INVALID_HANDLE`, and would skip the registry bookkeeping `nt_close_hook` owns.
//!
//! That covers `nt_close_hook`, `nt_cancel_io_file_hook` and `nt_wait_for_single_object_hook`.
//!
//! `nt_unlock_file_hook` is the odd one out, and deliberately so. It reads `MANAGED_HANDLES`
//! only to log that locking is not remoted, then calls the original either way, so annotating
//! it would change no behaviour. It is left unannotated to match the other three
//! handle-dispatching hooks, even though its twin `nt_lock_file_hook` is annotated.
//!
//! `connectex_detour` cannot be annotated at all: it is installed by pointer substitution in
//! `wsa_ioctl_detour` rather than by `apply_hook!`, and its original lives in `socket::ops`
//! behind `get_connectex_original()`. Its parent `wsa_ioctl_detour` is annotated, so an
//! internal thread never reaches the substitution in the first place.

use std::cell::Cell;

thread_local! {
    static INTERNAL: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard that marks the current thread as internal for its lifetime.
///
/// It restores the previous value rather than clearing, so a nested guard cannot unmark a
/// thread that an outer guard still owns.
pub struct InternalGuard {
    previous: bool,
}

impl InternalGuard {
    /// Mark the calling thread as internal until the guard is dropped.
    pub fn enter() -> Self {
        InternalGuard {
            previous: INTERNAL.replace(true),
        }
    }
}

impl Drop for InternalGuard {
    fn drop(&mut self) {
        INTERNAL.set(self.previous);
    }
}

/// Whether the current thread is a mirrord internal worker.
pub fn is_internal() -> bool {
    INTERNAL.get()
}

//! Marks mirrord's own threads so hooked APIs pass their traffic through.
//!
//! The layer enables every hook family inside `DllMain`, so the hooks are live before the
//! target reaches its entry point. Mirrord's own work still has to reach the internal proxy, the
//! crash monitor and the local disk afterwards. Without this marker its `socket`/`connect`/file
//! calls are intercepted and routed back through mirrord, which is wrong in three places:
//!
//! - the startup worker, whose traffic *is* the proxy connection being established,
//! - the crash handler, which must write its report to the local disk, allocation-free, on a
//!   faulting thread,
//! - the `CreateProcess` hook's own injection work, which reads the layer DLL and talks to the
//!   crash monitor on one of the target's threads.
//!
//! The `task_pool` workers are deliberately not marked: their jobs run application callbacks.
//!
//! It lives in `utils-win` rather than `layer-win` because the crash handler is here and
//! `utils-win` cannot depend on `layer-win`. `layer-win` re-exports it as
//! `crate::hooks::internal_thread`, which is the path the `internal_bypass` macro expands to.
//!
//! This marker is one of the two questions that macro asks. The other is "is a detour already
//! running on this thread?", which `mirrord_layer_lib::detour::DetourGuard` answers for one call.
//! A thread marker is right only for a thread that runs no other code; use the per-call guard
//! everywhere else.
//!
//! A hook that dispatches on *what it was given* rather than on who calls must serve a value the
//! layer handed out whoever passes it back. Each such hook says so in its own comment.

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
#[inline]
pub fn is_internal() -> bool {
    INTERNAL.get()
}

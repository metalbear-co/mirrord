//! Marks mirrord's own worker threads so hooked APIs pass their traffic through.
//!
//! The layer enables every hook family synchronously inside `DllMain` (before any user
//! code can run), but the layer's own worker thread still has to reach the internal
//! proxy afterwards. Without this marker that connection's `socket`/`connect`/file
//! calls would be intercepted and routed back through the not-yet-established
//! connection.

use std::cell::Cell;

thread_local! {
    static INTERNAL: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard that marks the current thread as internal for its lifetime.
pub(crate) struct InternalGuard;

impl InternalGuard {
    /// Mark the calling thread as internal until the guard is dropped.
    pub(crate) fn enter() -> Self {
        INTERNAL.set(true);
        InternalGuard
    }
}

impl Drop for InternalGuard {
    fn drop(&mut self) {
        INTERNAL.set(false);
    }
}

/// Whether the current thread is a mirrord internal worker.
pub(crate) fn is_internal() -> bool {
    INTERNAL.get()
}

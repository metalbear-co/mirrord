//! Sends a hooked call that the layer makes from inside a detour straight to the original
//! function.
//!
//! This is the Windows port of the unix layer's `DETOUR_BYPASS` and its `DetourGuard`
//! (`layer-lib/src/detour.rs`, applied by `#[hook_guard_fn]`). It answers a different question
//! from [`internal_thread`](crate::hooks::internal_thread):
//!
//! - `internal_thread` asks "is this thread mirrord's own?". The answer holds for the whole life of
//!   a thread, which is right for the startup worker and the crash handler, and wrong for a thread
//!   that also runs other people's code.
//! - This module asks "is a detour already running on this thread?". The answer holds for one call,
//!   so a thread that belongs to the target keeps its hooks for everything except the layer's own
//!   nested calls.
//!
//! A detour body reads the configuration, allocates, logs, and talks to the internal proxy. Each
//! of those can reach an API this layer hooks. Without this guard such a call comes back into the
//! hook, and the layer answers its own request with remote state.
//!
//! # A callback into application code must leave the guard
//!
//! [`ApplicationCallback`] exists because one detour body calls back into the target: the
//! completion routine in `socket::addrinfo_ex`. Application code called with the guard held would
//! bypass every hook, and a `FreeAddrInfoExW` from it would hand a chain this layer allocated to
//! `ws2_32`, which frees it with the wrong allocator. That is the `0xC0000374` heap corruption
//! that the thread-wide marker caused on the task-pool workers. Hold the guard for the layer's
//! own work, and leave it for the target's.
//!
//! # Thread-local safety
//!
//! `IN_DETOUR` is a `const`-initialized [`Cell`] with no destructor, the same shape
//! `internal_thread` uses. It needs no lazy initialization and registers no destructor, so
//! reading it cannot panic on a thread whose other storage is already gone. A `thread_local!`
//! that needed either would end the process from inside a detour.

use std::cell::Cell;

thread_local! {
    /// Whether a detour body is already running on this thread.
    ///
    /// Do not read or write this directly. Use [`BypassGuard::enter`], which the
    /// `internal_bypass` macro expands to, and [`ApplicationCallback::enter`].
    static IN_DETOUR: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard that marks a detour body as running on this thread.
///
/// [`enter`](Self::enter) gives `None` when a detour is already running, which is the caller's
/// signal to call the original function instead of its own body.
pub(crate) struct BypassGuard;

impl BypassGuard {
    /// Marks the calling thread until the guard is dropped.
    ///
    /// # Returns
    ///
    /// `Some` when this is the outermost detour on the thread, `None` when a detour already runs.
    /// The mark stays with the outermost guard either way.
    pub(crate) fn enter() -> Option<Self> {
        if IN_DETOUR.replace(true) {
            None
        } else {
            Some(BypassGuard)
        }
    }
}

impl Drop for BypassGuard {
    fn drop(&mut self) {
        // Only the outermost call holds a guard, so clearing is correct. A nested call got `None`
        // and has nothing to drop.
        IN_DETOUR.set(false);
    }
}

/// RAII guard that clears the mark for a call into application code.
///
/// Hold this around any call from a detour body into the target's own code. The target must see
/// the hooks that the rest of the process sees. See the warning at the top of this module for
/// what happens when it does not.
pub(crate) struct ApplicationCallback {
    previous: bool,
}

impl ApplicationCallback {
    /// Clears the mark until the guard is dropped, then restores what it was.
    pub(crate) fn enter() -> Self {
        ApplicationCallback {
            previous: IN_DETOUR.replace(false),
        }
    }
}

impl Drop for ApplicationCallback {
    fn drop(&mut self) {
        IN_DETOUR.set(self.previous);
    }
}

/// Whether a detour body is already running on this thread.
#[cfg(test)]
pub(crate) fn is_in_detour() -> bool {
    IN_DETOUR.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outermost call gets the guard and every nested call is told to bypass.
    #[test]
    fn nested_entry_is_refused() {
        assert!(!is_in_detour());

        let outer = BypassGuard::enter().expect("the outermost call owns the guard");
        assert!(is_in_detour());
        assert!(
            BypassGuard::enter().is_none(),
            "a nested call must be told to call the original"
        );

        drop(outer);
        assert!(!is_in_detour(), "the outermost guard clears the mark");
    }

    /// A refused guard must not clear the mark when it goes out of scope.
    #[test]
    fn a_refused_guard_leaves_the_mark_alone() {
        let _outer = BypassGuard::enter().expect("guard");

        {
            let nested = BypassGuard::enter();
            assert!(nested.is_none());
        }

        assert!(is_in_detour(), "the mark belongs to the outer guard");
    }

    /// Application code must run with the hooks the rest of the process sees.
    #[test]
    fn a_callback_leaves_and_restores_the_mark() {
        let _outer = BypassGuard::enter().expect("guard");

        {
            let _callback = ApplicationCallback::enter();
            assert!(!is_in_detour(), "application code sees no mark");
            assert!(
                BypassGuard::enter().is_some(),
                "a hook reached from application code runs its own body"
            );
        }

        assert!(is_in_detour(), "the layer's own work is marked again");
    }

    /// The escape is harmless where there is nothing to leave, which is the task-pool worker.
    #[test]
    fn a_callback_outside_a_detour_changes_nothing() {
        assert!(!is_in_detour());

        {
            let _callback = ApplicationCallback::enter();
            assert!(!is_in_detour());
        }

        assert!(!is_in_detour());
    }

    /// The mark is per thread. One thread inside a detour must not silence another's hooks.
    #[test]
    fn the_mark_does_not_cross_threads() {
        let _outer = BypassGuard::enter().expect("guard");
        assert!(is_in_detour());

        let other = std::thread::spawn(|| {
            assert!(!is_in_detour());
            BypassGuard::enter().is_some()
        });

        assert!(other.join().expect("probe thread"));
    }
}

/// Proves that `#[internal_bypass]` wires both marks up, which the guard's own tests cannot show.
///
/// Each test owns its statics, so the test harness can run them in parallel.
#[cfg(test)]
mod macro_contract {
    use std::sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    };

    use utils_win::internal_thread::InternalGuard;

    type ProbeFn = unsafe extern "system" fn(u32) -> u32;

    /// A detour body that calls the hooked API again must reach the original the second time.
    mod nested {
        use super::*;

        static ORIGINAL_CALLS: AtomicUsize = AtomicUsize::new(0);
        static BODY_CALLS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "system" fn original(value: u32) -> u32 {
            ORIGINAL_CALLS.fetch_add(1, Ordering::Relaxed);
            value
        }

        static ORIGINAL_IMPL: ProbeFn = original;
        static PROBE_ORIGINAL: OnceLock<&'static ProbeFn> = OnceLock::new();

        #[mirrord_layer_macro::internal_bypass(PROBE_ORIGINAL)]
        unsafe extern "system" fn probe_detour(value: u32) -> u32 {
            BODY_CALLS.fetch_add(1, Ordering::Relaxed);

            // Stands for the hooked API that a real detour body reaches through the layer's own
            // work: a log write, a configuration read, a proxy round-trip.
            unsafe { probe_detour(value) }
        }

        #[test]
        fn a_nested_call_reaches_the_original() {
            let _ = PROBE_ORIGINAL.set(&ORIGINAL_IMPL);

            assert_eq!(unsafe { probe_detour(7) }, 7);
            assert_eq!(BODY_CALLS.load(Ordering::Relaxed), 1, "one body run");
            assert_eq!(
                ORIGINAL_CALLS.load(Ordering::Relaxed),
                1,
                "the nested call must reach the original"
            );

            // The mark is released with the call, so the next top-level call runs the body again.
            assert_eq!(unsafe { probe_detour(9) }, 9);
            assert_eq!(BODY_CALLS.load(Ordering::Relaxed), 2);
            assert_eq!(ORIGINAL_CALLS.load(Ordering::Relaxed), 2);
        }
    }

    /// A thread that mirrord owns must never run a body.
    mod internal {
        use super::*;

        static ORIGINAL_CALLS: AtomicUsize = AtomicUsize::new(0);
        static BODY_CALLS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "system" fn original(value: u32) -> u32 {
            ORIGINAL_CALLS.fetch_add(1, Ordering::Relaxed);
            value
        }

        static ORIGINAL_IMPL: ProbeFn = original;
        static PROBE_ORIGINAL: OnceLock<&'static ProbeFn> = OnceLock::new();

        #[mirrord_layer_macro::internal_bypass(PROBE_ORIGINAL)]
        unsafe extern "system" fn probe_detour(value: u32) -> u32 {
            BODY_CALLS.fetch_add(1, Ordering::Relaxed);
            value
        }

        #[test]
        fn a_marked_thread_reaches_the_original() {
            let _ = PROBE_ORIGINAL.set(&ORIGINAL_IMPL);

            let marked = InternalGuard::enter();
            assert_eq!(unsafe { probe_detour(1) }, 1);
            assert_eq!(BODY_CALLS.load(Ordering::Relaxed), 0, "no body run");
            assert_eq!(ORIGINAL_CALLS.load(Ordering::Relaxed), 1);

            drop(marked);

            assert_eq!(unsafe { probe_detour(2) }, 2);
            assert_eq!(
                BODY_CALLS.load(Ordering::Relaxed),
                1,
                "an unmarked thread runs the body"
            );
            assert_eq!(ORIGINAL_CALLS.load(Ordering::Relaxed), 1);
        }
    }
}

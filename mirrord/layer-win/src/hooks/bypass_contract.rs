//! Proves that `#[internal_bypass]` wires both marks up, which the guard's own tests in
//! `layer-lib` cannot show.
//!
//! Each test owns its statics, so the test harness can run them in parallel.

use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

use mirrord_layer_lib::detour::DetourGuard;
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

/// A value the layer handed out is served by the body whoever passes it back, and everything
/// else keeps both bypasses. `managed` is asked only when a bypass is on the table.
mod managed {
    use std::sync::atomic::AtomicBool;

    use super::*;

    const MANAGED_VALUE: u32 = 0x5000_0001;

    static ORIGINAL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static BODY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static MANAGED_ASKED: AtomicUsize = AtomicUsize::new(0);
    static BODY_SAW_MARK: AtomicBool = AtomicBool::new(true);

    unsafe extern "system" fn original(value: u32) -> u32 {
        ORIGINAL_CALLS.fetch_add(1, Ordering::Relaxed);
        value
    }

    static ORIGINAL_IMPL: ProbeFn = original;
    static PROBE_ORIGINAL: OnceLock<&'static ProbeFn> = OnceLock::new();

    fn is_managed(value: u32) -> bool {
        MANAGED_ASKED.fetch_add(1, Ordering::Relaxed);
        value == MANAGED_VALUE
    }

    #[mirrord_layer_macro::internal_bypass(PROBE_ORIGINAL, managed = is_managed(value))]
    unsafe extern "system" fn probe_detour(value: u32) -> u32 {
        BODY_CALLS.fetch_add(1, Ordering::Relaxed);
        if !DetourGuard::is_held() {
            BODY_SAW_MARK.store(false, Ordering::Relaxed);
        }
        value
    }

    /// One test, because the calls share the counters.
    #[test]
    fn a_managed_value_reaches_the_body_from_anywhere() {
        let _ = PROBE_ORIGINAL.set(&ORIGINAL_IMPL);

        // An ordinary call runs the body without asking.
        assert_eq!(unsafe { probe_detour(3) }, 3);
        assert_eq!(BODY_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(
            MANAGED_ASKED.load(Ordering::Relaxed),
            0,
            "a call that runs the body anyway pays for no lookup"
        );

        // An internal thread: the managed value runs the body, anything else is passed on.
        let marked = InternalGuard::enter();
        assert_eq!(unsafe { probe_detour(MANAGED_VALUE) }, MANAGED_VALUE);
        assert_eq!(unsafe { probe_detour(1) }, 1);
        drop(marked);
        assert_eq!(BODY_CALLS.load(Ordering::Relaxed), 2);
        assert_eq!(ORIGINAL_CALLS.load(Ordering::Relaxed), 1);
        assert!(!DetourGuard::is_held(), "the body's own guard is released");

        // Inside another detour: same split, and the enclosing mark stays set.
        let outer = DetourGuard::new().expect("the outermost call owns the guard");
        assert_eq!(unsafe { probe_detour(MANAGED_VALUE) }, MANAGED_VALUE);
        assert_eq!(unsafe { probe_detour(2) }, 2);
        assert!(
            DetourGuard::is_held(),
            "the enclosing detour still holds the mark"
        );
        drop(outer);
        assert_eq!(BODY_CALLS.load(Ordering::Relaxed), 3);
        assert_eq!(ORIGINAL_CALLS.load(Ordering::Relaxed), 2);
        assert_eq!(MANAGED_ASKED.load(Ordering::Relaxed), 4);

        assert!(
            BODY_SAW_MARK.load(Ordering::Relaxed),
            "the body's nested calls must reach the originals"
        );
    }
}

/// The tracing that `instrument` adds runs only for a body run, and inside the mark.
///
/// `mirrord_layer_macro::instrument` only expands with debug assertions.
#[cfg(debug_assertions)]
mod traced {
    use std::sync::atomic::AtomicBool;

    use tracing::{
        Event, Metadata, Subscriber,
        span::{Attributes, Id, Record},
    };

    use super::*;

    static ORIGINAL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static SPANS: AtomicUsize = AtomicUsize::new(0);
    static EVENTS: AtomicUsize = AtomicUsize::new(0);
    static EVENT_OUTSIDE_MARK: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn original(value: u32) -> u32 {
        ORIGINAL_CALLS.fetch_add(1, Ordering::Relaxed);
        value
    }

    static ORIGINAL_IMPL: ProbeFn = original;
    static PROBE_ORIGINAL: OnceLock<&'static ProbeFn> = OnceLock::new();

    #[mirrord_layer_macro::internal_bypass(PROBE_ORIGINAL)]
    #[mirrord_layer_macro::instrument(level = "trace", ret)]
    unsafe extern "system" fn probe_detour(value: u32) -> u32 {
        // The hooked API reached again through the body's own work opens no span.
        unsafe { probe_detour(value) };
        value
    }

    /// Records the probe's spans and events.
    struct RecordingSubscriber;

    impl Subscriber for RecordingSubscriber {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, span: &Attributes<'_>) -> Id {
            if span.metadata().name() == "probe_detour" {
                SPANS.fetch_add(1, Ordering::Relaxed);
            }
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, _: &Event<'_>) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
            if !DetourGuard::is_held() {
                EVENT_OUTSIDE_MARK.store(true, Ordering::Relaxed);
            }
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    #[test]
    fn tracing_runs_inside_the_mark() {
        let _ = PROBE_ORIGINAL.set(&ORIGINAL_IMPL);

        tracing::subscriber::with_default(RecordingSubscriber, || {
            assert_eq!(unsafe { probe_detour(7) }, 7);
            assert_eq!(
                SPANS.load(Ordering::Relaxed),
                1,
                "one span, named after the hook, for the one body run"
            );
            assert_eq!(ORIGINAL_CALLS.load(Ordering::Relaxed), 1);
            assert!(EVENTS.load(Ordering::Relaxed) >= 1, "the `ret` event");
            assert!(
                !EVENT_OUTSIDE_MARK.load(Ordering::Relaxed),
                "the `ret` event is written while the mark is held"
            );

            // A thread mirrord owns reaches the original without a span.
            let marked = InternalGuard::enter();
            assert_eq!(unsafe { probe_detour(9) }, 9);
            drop(marked);
            assert_eq!(SPANS.load(Ordering::Relaxed), 1, "no span for a bypass");
            assert_eq!(ORIGINAL_CALLS.load(Ordering::Relaxed), 2);
        });
    }
}

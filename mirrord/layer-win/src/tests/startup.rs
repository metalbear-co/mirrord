//! The layer's startup contract when its startup fails: the waiting parent learns of it at once,
//! the crash monitor gets a report even before the layer registered, and the process ends.
//!
//! Each case runs the layer's code in a fresh copy of this test binary, because the code under
//! test ends its process and flips process-wide state (the proxy connection gate, the crash
//! handler) that no other test may see. The parent half plays the launcher and the monitor.

use std::{
    ffi::c_void,
    io::{Read, Write},
    net::TcpListener,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    process::{Child, Command, Stdio},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use mirrord_config::MIRRORD_LAYER_CRASH_MONITOR_ADDR;
use mirrord_layer_lib::process::windows::sync::{
    ChildInitEvent, InitWaitOutcome, ParentInitEvents,
};
use winapi::{
    shared::minwindef::FALSE,
    um::{processthreadsapi::OpenProcess, winnt::SYNCHRONIZE},
};

/// Set in the copy of the test binary that runs the layer's half.
const ISOLATED: &str = "MIRRORD_LAYER_WIN_ISOLATED_TEST";

/// Starts the ignored test `name` in a fresh copy of this test binary.
fn spawn_isolated(name: &str, environment: &[(&str, String)]) -> Child {
    Command::new(std::env::current_exe().expect("test binary"))
        .args([
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ISOLATED, "1")
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the isolated test")
}

/// What the layer's half does first: claim the readiness event the way `DllMain` does, once the
/// parent half has created it.
fn claim_like_dll_main() -> ChildInitEvent {
    assert!(
        std::env::var_os(ISOLATED).is_some(),
        "runs only in the copy `spawn_isolated` starts"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(event) = ChildInitEvent::open() {
            return event;
        }
        assert!(
            Instant::now() < deadline,
            "the parent never created the events"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Plays the launcher: creates the child's events, and waits for what the layer reports.
fn wait_like_a_launcher(child: &Child) -> InitWaitOutcome {
    let events = ParentInitEvents::create(child.id()).expect("create the child's events");
    let process = unsafe { OpenProcess(SYNCHRONIZE, FALSE, child.id()) };
    assert!(!process.is_null(), "open the child");
    let process = unsafe { OwnedHandle::from_raw_handle(process.cast()) };
    events
        .wait(
            unsafe { BorrowedHandle::borrow_raw(process.as_raw_handle()) },
            Some(20_000),
        )
        .expect("wait for the child")
}

/// A crash monitor that takes one registration, acknowledges it, and hands back its bytes.
fn fake_monitor() -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address").to_string();
    let monitor = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a registration");
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).expect("length");
        let mut registration = vec![0u8; u32::from_le_bytes(length) as usize];
        stream.read_exact(&mut registration).expect("registration");
        stream.write_all(&[1]).expect("ack");
        registration
    });
    (address, monitor)
}

/// A panic in the asynchronous startup, before the layer registered with the crash monitor, is a
/// failure like any other: the parent reads it from the failure event, the monitor gets the
/// failure in a registration of its own, and the process ends.
#[cfg(debug_assertions)]
#[test]
fn a_panic_before_registration_is_reported_and_ends_the_process() {
    let (monitor_address, monitor) = fake_monitor();
    let child = spawn_isolated(
        "tests::startup::isolated_panicking_startup",
        &[
            (MIRRORD_LAYER_CRASH_MONITOR_ADDR, monitor_address),
            (
                "MIRRORD_LAYER_DEBUG_PANIC_BEFORE_REGISTRATION",
                "1".to_owned(),
            ),
        ],
    );

    let outcome = wait_like_a_launcher(&child);
    let registration = monitor.join().expect("monitor");
    let output = child.wait_with_output().expect("child");

    assert_eq!(
        outcome,
        InitWaitOutcome::Failed,
        "the parent is told at once"
    );
    let registration = String::from_utf8_lossy(&registration);
    assert!(
        registration.contains("panicked")
            && registration.contains("MIRRORD_LAYER_DEBUG_PANIC_BEFORE_REGISTRATION"),
        "the monitor got the panic: {registration}"
    );
    assert_eq!(output.status.code(), Some(1), "the process ends");
}

#[cfg(debug_assertions)]
#[test]
#[ignore = "the layer's half of a_panic_before_registration_is_reported_and_ends_the_process"]
fn isolated_panicking_startup() {
    let ready = claim_like_dll_main();
    crate::run_startup_worker(Some(ready));
    unreachable!("a failed startup ends the process");
}

/// A startup worker that could not be started leaves the hooks with no connection to wait for.
/// The layer turns them off again, so the process runs as if mirrord had never loaded, and the
/// parent is told at once rather than at its timeout.
#[test]
fn a_worker_that_cannot_start_is_reported_to_the_parent() {
    let child = spawn_isolated("tests::startup::isolated_worker_not_started", &[]);

    let outcome = wait_like_a_launcher(&child);
    let output = child.wait_with_output().expect("child");

    assert_eq!(outcome, InitWaitOutcome::Failed);
    assert!(
        output.status.success(),
        "the layer's half passed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "the layer's half of a_worker_that_cannot_start_is_reported_to_the_parent"]
fn isolated_worker_not_started() {
    let _ready = claim_like_dll_main();
    let guard = crate::initialize_detour_guard().expect("the hook engine");
    guard
        .create_hook::<HookedFn>(
            hooked_target as *const () as *mut c_void,
            hooked_detour as *const () as *mut c_void,
        )
        .expect("create the hook");
    guard.enable_all_hooks().expect("enable the hook");
    assert_eq!(call_hooked_target(), DETOURED, "the hook is live");

    crate::startup_worker_not_started();

    assert_eq!(
        call_hooked_target(),
        ORIGINAL,
        "the original runs once the worker could not start"
    );
}

/// What the hooked function answers, unhooked and hooked.
const ORIGINAL: u32 = 1;
const DETOURED: u32 = 2;

type HookedFn = extern "system" fn() -> u32;

/// The function `isolated_worker_not_started` hooks, the way the layer hooks a Win32 function.
#[inline(never)]
extern "system" fn hooked_target() -> u32 {
    std::hint::black_box(ORIGINAL)
}

extern "system" fn hooked_detour() -> u32 {
    DETOURED
}

/// Calls [`hooked_target`] through a pointer the compiler cannot see through, so the call goes
/// through the patched function rather than being inlined or folded.
fn call_hooked_target() -> u32 {
    let target: HookedFn = std::hint::black_box(hooked_target);
    target()
}

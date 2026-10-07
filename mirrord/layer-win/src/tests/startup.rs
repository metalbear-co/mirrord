//! The layer's startup contract. When its startup fails, the waiting parent learns of it at once,
//! the crash monitor gets a report even before the layer registered, and the process ends. When it
//! registers, it names a log the monitor can open.
//!
//! Each case runs the layer's code in a fresh copy of this test binary, because the code under
//! test ends its process and flips process-wide state (the proxy connection gate, the crash
//! handler, the log file, the working directory) that no other test may see. The parent half plays
//! the launcher and the monitor.

use std::{
    ffi::c_void,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
#[cfg(debug_assertions)]
use std::{io::Write, net::TcpListener, thread::JoinHandle};

use mirrord_config::MIRRORD_LAYER_CRASH_MONITOR_ADDR;
use mirrord_layer_lib::{
    logging::{MIRRORD_LAYER_LOG_PATH, init_tracing_sinks},
    process::windows::{
        diagnostics::session_role,
        injection::{InjectionMethod, MIRRORD_INJECTION_METHOD_ENV},
        sync::{ChildInitEvent, InitWaitOutcome, ParentInitEvents},
    },
};
use tracing::subscriber::{NoSubscriber, with_default};
#[cfg(debug_assertions)]
use utils_win::diagnostics::monitor::{ACK_READY, InitReport, Registration, read_registration};
use utils_win::diagnostics::{crash_dir, monitor::is_log_name};
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

/// A crash monitor that takes one registration, acknowledges it, and hands it back.
#[cfg(debug_assertions)]
fn fake_monitor() -> (String, JoinHandle<Registration>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address").to_string();
    let monitor = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a registration");
        let registration = read_registration(&mut stream).expect("registration");
        stream.write_all(&[ACK_READY]).expect("ack");
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
    match registration.init_report {
        Some(InitReport::Failed(reason)) => assert!(
            reason.contains("panicked")
                && reason.contains("MIRRORD_LAYER_DEBUG_PANIC_BEFORE_REGISTRATION"),
            "the monitor got the panic: {reason}"
        ),
        report => panic!("the monitor got {report:?}"),
    }
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

/// The method a layer's children are injected with is the one that loaded it, captured at startup
/// whatever the log level. A capture that depended on an event being logged would happen only at
/// the first child launch, and would see a variable the target changed in the meantime.
#[test]
fn startup_captures_the_injection_method_with_logging_off() {
    let child = spawn_isolated(
        "tests::startup::isolated_injection_method_capture",
        &[(MIRRORD_INJECTION_METHOD_ENV, "apc".to_owned())],
    );

    let output = child.wait_with_output().expect("child");

    assert!(
        output.status.success(),
        "the layer's half passed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "the layer's half of startup_captures_the_injection_method_with_logging_off"]
fn isolated_injection_method_capture() {
    assert!(
        std::env::var_os(ISOLATED).is_some(),
        "runs only in the copy `spawn_isolated` starts"
    );

    // No resolved configuration, so startup stops after the capture, before any hook.
    let started = with_default(NoSubscriber::default(), crate::initialize_layer_sync);
    assert!(
        started.is_err(),
        "startup stops at the missing configuration"
    );

    // SAFETY: this copy of the test binary runs this one test on one thread.
    unsafe { std::env::set_var(MIRRORD_INJECTION_METHOD_ENV, "load-library") };
    assert_eq!(
        crate::hooks::process::init_layer_injection_method(),
        InjectionMethod::Apc
    );
}

/// The working directory the layer's half of
/// `the_monitor_finds_a_registered_log_from_another_working_directory` runs in.
const WORKING_DIRECTORY: &str = "MIRRORD_LAYER_WIN_TEST_WORKING_DIRECTORY";

/// The file, in [`WORKING_DIRECTORY`], the layer's half writes the log name it registered to.
const REGISTERED_LOG_NAME: &str = "registered-log-name";

/// The layer registers its log by name, and the monitor, which runs in a working directory of its
/// own, finds it in the session directory. A relative session directory names a different place
/// in each working directory, so a layer that sees one registers no log at all.
#[test]
fn the_monitor_finds_a_registered_log_from_another_working_directory() {
    let scratch = std::env::temp_dir().join(format!(
        "mirrord-layer-log-directory-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    let working_directory = scratch.join("layer");
    let session_directory = scratch.join("session");
    std::fs::create_dir_all(&working_directory).expect("the layer's working directory");

    let registered = |log_directory: &Path| {
        let _ = std::fs::remove_file(working_directory.join(REGISTERED_LOG_NAME));
        let child = spawn_isolated(
            "tests::startup::isolated_log_registration",
            &[
                (
                    MIRRORD_LAYER_LOG_PATH,
                    log_directory.to_string_lossy().into_owned(),
                ),
                // Only configured: building the registration connects to nothing.
                (MIRRORD_LAYER_CRASH_MONITOR_ADDR, "127.0.0.1:9".to_owned()),
                (
                    WORKING_DIRECTORY,
                    working_directory.to_string_lossy().into_owned(),
                ),
            ],
        );
        let output = child.wait_with_output().expect("child");
        assert!(
            output.status.success(),
            "the layer's half passed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_to_string(working_directory.join(REGISTERED_LOG_NAME)).ok()
    };

    // What the CLI gives every process of the session.
    let log_name = registered(&session_directory);
    // The monitor's half: its own working directory is this process's, not the layer's.
    let found = log_name.as_deref().is_some_and(|log_name| {
        is_log_name(log_name) && session_directory.join(log_name).is_file()
    });
    // What a process launched outside the CLI could see.
    let relative_log_name = registered(Path::new("logs"));
    let relative_log_written = working_directory.join("logs").is_dir();
    let _ = std::fs::remove_dir_all(&scratch);

    assert!(found, "the monitor finds the registered log {log_name:?}");
    assert_eq!(
        relative_log_name, None,
        "a log in a relative directory is not registered"
    );
    assert!(
        relative_log_written,
        "it is still written, in the layer's own working directory"
    );
}

#[test]
#[ignore = "the layer's half of the_monitor_finds_a_registered_log_from_another_working_directory"]
fn isolated_log_registration() {
    assert!(
        std::env::var_os(ISOLATED).is_some(),
        "runs only in the copy `spawn_isolated` starts"
    );
    std::env::set_current_dir(std::env::var_os(WORKING_DIRECTORY).expect("a working directory"))
        .expect("enter the working directory");

    init_tracing_sinks();
    let (_, registration) =
        crate::diagnostics::monitor_registration(&session_role(), "child.exe", &crash_dir())
            .expect("a monitor is configured");

    if let Some(log_name) = registration.log_name {
        std::fs::write(REGISTERED_LOG_NAME, log_name).expect("hand over the log name");
    }
}

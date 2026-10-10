//! Windows counterparts of the supervision tests in `process.rs`.
//!
//! Windows teardown has no graceful phase: [`Service::stop`] terminates the service's Job Object.
//! So instead of checking that a service saw a signal, these tests check that every process in a
//! service tree is gone. Every process they start is this test binary, re-executed into
//! [`child_helper`] or [`supervisor_helper`], so they need no shell and learn the PIDs of the
//! processes they must see exit.
//!
//! Each Job Object is also kill-on-close, so dropping a service ends its tree too. The tests show
//! that every process is gone once supervision returns, not which of the two ended it.

use std::{
    fs,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
};

use rstest::rstest;
use tempfile::TempDir;
use tokio::process::Child;
use winapi::{
    shared::minwindef::{BOOL, DWORD, FALSE, TRUE},
    um::{
        consoleapi::SetConsoleCtrlHandler,
        handleapi::CloseHandle,
        processenv::GetStdHandle,
        processthreadsapi::OpenProcess,
        synchapi::WaitForSingleObject,
        winbase::{CREATE_NO_WINDOW, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, WAIT_OBJECT_0},
        wincon::{CTRL_BREAK_EVENT, CTRL_C_EVENT, GenerateConsoleCtrlEvent},
        winnt::SYNCHRONIZE,
    },
};

use super::{
    test_support::{TEST_GRACE, TIMEOUT, wait_for_file, wait_for_helper_ready},
    *,
};

/// Directory the helpers write their PID files to and watch for trigger files in.
const DIRECTORY_ENV: &str = "MIRRORD_UP_TEST_DIRECTORY";
/// Name of a [`child_helper`], used for its PID file and as the service name.
const NAME_ENV: &str = "MIRRORD_UP_TEST_NAME";
/// What a [`child_helper`] does, see there.
const MODE_ENV: &str = "MIRRORD_UP_TEST_MODE";
/// Mode of the one service a [`supervisor_helper`] runs.
const SERVICE_MODE_ENV: &str = "MIRRORD_UP_TEST_SERVICE_MODE";
/// Makes a [`supervisor_helper`] keep running after its services are gone.
const DRAIN_ENV: &str = "MIRRORD_UP_TEST_DRAIN";

/// Arguments that re-execute this test binary into the ignored test `helper` alone.
///
/// `--quiet` matters: run serially, libtest's default format prints `test <name> ... ` without a
/// newline before the test starts, which would glue onto the helper's session-ready line.
fn helper_args(helper: &str) -> [String; 5] {
    [
        "--exact".to_owned(),
        format!("process::windows_tests::{helper}"),
        "--ignored".to_owned(),
        "--nocapture".to_owned(),
        "--quiet".to_owned(),
    ]
}

fn helper(helper: &str, directory: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(helper_args(helper))
        .env(DIRECTORY_ENV, directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// A service called `name` that behaves as `mode` says (see [`child_helper`]).
fn service(directory: &Path, name: &str, mode: &str) -> (Arc<str>, Command) {
    let mut command = helper("child_helper", directory);
    command.env(NAME_ENV, name).env(MODE_ENV, mode);
    (Arc::from(name), command)
}

fn wait_for_file_blocking(path: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    while path.exists().not() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Writes `path` under a temporary name first, so that a test polling for it never reads it
/// half-written.
fn write_atomically(path: &Path, contents: impl AsRef<[u8]>) {
    let partial = path.with_extension("partial");
    fs::write(&partial, contents).unwrap();
    fs::rename(&partial, path).unwrap();
}

/// A process opened while it was known to be running, so that checking on it later cannot be
/// fooled by its PID being reused.
struct Process(OwnedHandle);

impl Process {
    /// Waits for the helper called `name` to write its PID file, then opens it.
    async fn wait_for(directory: &Path, name: &str) -> Self {
        let path = directory.join(format!("{name}.pid"));
        wait_for_file(&path).await;
        let pid = fs::read_to_string(&path).unwrap().parse().unwrap();
        let handle = unsafe { OpenProcess(SYNCHRONIZE, FALSE, pid) };
        assert!(
            handle.is_null().not(),
            "failed to open {name}: {}",
            io::Error::last_os_error()
        );
        Self(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
    }

    /// Blocks until the process exits, for up to [`TIMEOUT`], and says whether it did.
    fn exits(&self) -> bool {
        let handle = self.0.as_raw_handle().cast();
        unsafe { WaitForSingleObject(handle, TIMEOUT.as_millis() as DWORD) == WAIT_OBJECT_0 }
    }
}

/// Entry point of every service these tests supervise. Once running, each mode writes
/// `<name>.pid` and, unless it is `silent`, prints the session-ready line. Then:
///
/// - `serve`: keeps running.
/// - `serve-ignoring-ctrl-c`: keeps running, through any console event.
/// - `tree`: starts a `serve` grandchild called `<name>-grandchild` first, then keeps running.
/// - `tree-exit`: like `tree`, but exits with 0 once the test creates `release`.
/// - `crash`: exits with 7 once the test creates `release`.
/// - `exit`: exits with 0.
/// - `silent`: closes its stdout and stderr first, then keeps running.
#[test]
#[ignore = "subprocess entry point for Windows supervision tests"]
fn child_helper() {
    let Some(directory) = std::env::var_os(DIRECTORY_ENV) else {
        return;
    };
    let directory = Path::new(&directory);
    let name = std::env::var(NAME_ENV).unwrap();
    let mode = std::env::var(MODE_ENV).unwrap();

    match mode.as_str() {
        "serve-ignoring-ctrl-c" => {
            unsafe extern "system" fn ignore(_event: DWORD) -> BOOL {
                TRUE
            }
            assert_ne!(unsafe { SetConsoleCtrlHandler(Some(ignore), TRUE) }, 0);
        }
        "tree" | "tree-exit" => {
            let grandchild = format!("{name}-grandchild");
            // Never waited for on purpose: only the service's Job Object may end the grandchild.
            #[allow(clippy::zombie_processes)]
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(helper_args("child_helper"))
                .env(NAME_ENV, &grandchild)
                .env(MODE_ENV, "serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            wait_for_file_blocking(&directory.join(format!("{grandchild}.pid")));
        }
        "silent" => unsafe {
            CloseHandle(GetStdHandle(STD_OUTPUT_HANDLE));
            CloseHandle(GetStdHandle(STD_ERROR_HANDLE));
        },
        "serve" | "crash" | "exit" => {}
        other => panic!("unknown child_helper mode {other}"),
    }

    write_atomically(
        &directory.join(format!("{name}.pid")),
        std::process::id().to_string(),
    );
    if mode != "silent" {
        println!("{SESSION_READY_MESSAGE}");
    }

    match mode.as_str() {
        "tree-exit" => wait_for_file_blocking(&directory.join("release")),
        "crash" => {
            wait_for_file_blocking(&directory.join("release"));
            std::process::exit(7);
        }
        "exit" => {}
        _ => std::thread::sleep(Duration::from_secs(60)),
    }
}

/// Runs [`run`] as `mirrord up` does, with real console event handlers, over one service in
/// `MIRRORD_UP_TEST_SERVICE_MODE`. Each time the test writes a console event to `press`, it
/// generates that event for every process in its console, as a user's Ctrl-C or Ctrl-Break does.
#[tokio::test]
#[ignore = "subprocess entry point for Windows console event tests"]
async fn supervisor_helper() {
    let Some(directory) = std::env::var_os(DIRECTORY_ENV) else {
        return;
    };
    let directory = PathBuf::from(directory);
    let service_mode = std::env::var(SERVICE_MODE_ENV).unwrap();

    // A process can start with Ctrl-C ignored, inherited from whoever started the tests, and it
    // would then never see the event. A user's terminal does not start `mirrord up` that way.
    assert_ne!(unsafe { SetConsoleCtrlHandler(None, FALSE) }, 0);
    let press = directory.join("press");
    std::thread::spawn(move || {
        loop {
            while press.exists().not() {
                std::thread::sleep(Duration::from_millis(10));
            }
            let event = fs::read_to_string(&press).unwrap().parse().unwrap();
            fs::remove_file(&press).unwrap();
            if unsafe { GenerateConsoleCtrlEvent(event, 0) } == 0 {
                eprintln!(
                    "failed to generate console event {event}: {}",
                    io::Error::last_os_error()
                );
                std::process::exit(3);
            }
        }
    });

    let ready = ReadyTracker::default();
    run(
        vec![service(&directory, "service", &service_mode)],
        ready.clone(),
    )
    .await
    .unwrap();
    assert!(ready.time_to_ready().is_some());
    if std::env::var_os(DRAIN_ENV).is_some() {
        fs::write(directory.join("draining"), []).unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// Starts a [`supervisor_helper`] in a console of its own, so that its console events reach only it
/// and its service. `CREATE_NO_WINDOW` gives it that console without opening a window.
fn supervisor(directory: &Path, service_mode: &str, drain: bool) -> Child {
    let mut command = helper("supervisor_helper", directory);
    command
        .env(SERVICE_MODE_ENV, service_mode)
        .stderr(Stdio::inherit())
        .creation_flags(CREATE_NO_WINDOW);
    if drain {
        command.env(DRAIN_ENV, "1");
    }
    command.spawn().unwrap()
}

/// Has the [`supervisor_helper`] in `directory` generate `event` in its console.
fn press(directory: &Path, event: DWORD) {
    write_atomically(&directory.join("press"), event.to_string());
}

async fn wait_for_exit(supervisor: &mut Child) -> ExitStatus {
    tokio::time::timeout(TIMEOUT, supervisor.wait())
        .await
        .expect("supervisor did not exit")
        .unwrap()
}

#[tokio::test]
async fn cancellation_ends_the_whole_service_tree_and_keeps_readiness() {
    let directory = TempDir::new().unwrap();
    let ready = ReadyTracker::default();
    let mut tree = None;
    let shutdown = async {
        tree = Some((
            Process::wait_for(directory.path(), "tree").await,
            Process::wait_for(directory.path(), "tree-grandchild").await,
        ));
        tokio::time::timeout(TIMEOUT, async {
            while ready.time_to_ready().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("service never became ready");
        Ok(ShutdownSignal::CtrlC)
    };
    tokio::time::timeout(
        TIMEOUT,
        supervise(
            vec![service(directory.path(), "tree", "tree")],
            ready.clone(),
            shutdown,
            TEST_GRACE,
        ),
    )
    .await
    .expect("the service tree was not stopped")
    .unwrap();

    assert!(ready.time_to_ready().is_some());
    let (child, grandchild) = tree.unwrap();
    assert!(child.exits());
    assert!(
        grandchild.exits(),
        "the Job Object must end descendants too"
    );
}

#[tokio::test]
async fn cleanup_reaches_grandchild_that_outlives_the_service() {
    let directory = TempDir::new().unwrap();
    let mut grandchild = None;
    // The service exits on its own once released, so first-exit polling resolves the select
    // rather than this future, and only the Job Object can still reach the grandchild.
    let release = async {
        grandchild = Some(Process::wait_for(directory.path(), "tree-grandchild").await);
        fs::write(directory.path().join("release"), []).unwrap();
        std::future::pending::<io::Result<ShutdownSignal>>().await
    };
    tokio::time::timeout(
        TIMEOUT,
        supervise(
            vec![service(directory.path(), "tree", "tree-exit")],
            ReadyTracker::default(),
            release,
            TEST_GRACE,
        ),
    )
    .await
    .expect("the grandchild was not stopped")
    .expect("a service exiting on its own must not be reported as a crash");

    assert!(grandchild.unwrap().exits());
}

#[tokio::test]
async fn crash_remains_failure_and_stops_siblings() {
    let directory = TempDir::new().unwrap();
    let mut sibling = None;
    let release = async {
        sibling = Some(Process::wait_for(directory.path(), "sibling").await);
        fs::write(directory.path().join("release"), []).unwrap();
        std::future::pending::<io::Result<ShutdownSignal>>().await
    };
    let result = tokio::time::timeout(
        TIMEOUT,
        supervise(
            vec![
                service(directory.path(), "sibling", "serve"),
                service(directory.path(), "failing", "crash"),
            ],
            ReadyTracker::default(),
            release,
            TEST_GRACE,
        ),
    )
    .await
    .expect("the sibling was not stopped");

    assert!(matches!(
        result,
        Err(UpError::ServiceCrashed { name, status }) if &*name == "failing" && status.code() == Some(7)
    ));
    assert!(sibling.unwrap().exits());
}

#[tokio::test]
async fn spawn_failure_is_returned_instead_of_panicking() {
    let directory = TempDir::new().unwrap();
    let missing = (
        Arc::from("missing"),
        Command::new(directory.path().join("missing.exe")),
    );
    // Teardown starts before the spawned service can write its PID, so this only checks that
    // supervision returns. The service's Job Object is kill-on-close, so it ends either way.
    let result = tokio::time::timeout(
        TIMEOUT,
        supervise(
            vec![service(directory.path(), "valid", "serve"), missing],
            ReadyTracker::default(),
            std::future::pending(),
            TEST_GRACE,
        ),
    )
    .await
    .expect("the spawned service was not stopped");

    assert!(matches!(result, Err(UpError::Io(error)) if error.kind() == io::ErrorKind::NotFound));
}

#[tokio::test]
async fn closed_output_does_not_starve_cancellation() {
    let directory = TempDir::new().unwrap();
    let mut silent = None;
    let shutdown = async {
        silent = Some(Process::wait_for(directory.path(), "silent").await);
        Ok(ShutdownSignal::CtrlC)
    };
    tokio::time::timeout(
        TIMEOUT,
        supervise(
            vec![service(directory.path(), "silent", "silent")],
            ReadyTracker::default(),
            shutdown,
            TEST_GRACE,
        ),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(silent.unwrap().exits());
}

#[tokio::test]
async fn second_shutdown_error_forces_cleanup_before_returning() {
    let directory = TempDir::new().unwrap();
    let mut serving = None;
    let first_shutdown = async {
        serving = Some(Process::wait_for(directory.path(), "serving").await);
        Ok(ShutdownSignal::CtrlC)
    };
    let second_shutdown = async {
        wait_for_file(&directory.path().join("serving.pid")).await;
        Err(io::Error::other("shutdown signal stream closed"))
    };
    let result = tokio::time::timeout(
        TIMEOUT,
        supervise_with_second_signal(
            vec![service(directory.path(), "serving", "serve")],
            ReadyTracker::default(),
            first_shutdown,
            second_shutdown,
            TEST_GRACE,
        ),
    )
    .await
    .expect("second shutdown error did not force cleanup");

    assert!(matches!(result, Err(UpError::Io(error)) if error.kind() == io::ErrorKind::Other));
    assert!(serving.unwrap().exits());
}

#[rstest]
#[case::ctrl_c(CTRL_C_EVENT)]
#[case::ctrl_break(CTRL_BREAK_EVENT)]
#[tokio::test]
async fn console_event_ends_the_session_successfully_and_stops_services(#[case] event: DWORD) {
    let directory = TempDir::new().unwrap();
    let mut supervisor = supervisor(directory.path(), "serve-ignoring-ctrl-c", false);
    let service = Process::wait_for(directory.path(), "service").await;
    wait_for_helper_ready(&mut supervisor).await;

    press(directory.path(), event);
    let status = wait_for_exit(&mut supervisor).await;

    assert!(status.success(), "supervisor failed: {status}");
    assert!(service.exits());
}

#[tokio::test]
async fn ctrl_c_after_services_exit_still_ends_the_process() {
    let directory = TempDir::new().unwrap();
    let mut supervisor = supervisor(directory.path(), "exit", true);
    wait_for_helper_ready(&mut supervisor).await;
    wait_for_file(&directory.path().join("draining")).await;

    // The console handlers stay installed after supervision ends, so without the forced exit
    // this Ctrl-C would be swallowed and the process would keep running.
    press(directory.path(), CTRL_C_EVENT);
    let status = wait_for_exit(&mut supervisor).await;

    assert_eq!(status.code(), Some(130));
}

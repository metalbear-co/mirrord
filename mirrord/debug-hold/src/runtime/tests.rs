//! The port against a stand-in for a paused .NET runtime, over the real pipe or socket.
//!
//! [`FakeRuntime`] does what a runtime started with `DOTNET_DiagnosticPorts=<port>,connect,suspend`
//! does: it connects and advertises, answers the one command each connection carries, closes the
//! connection, and connects again at once. Its replies are the recorded frames of a real .NET 8
//! runtime, or ones built the same way.

#[cfg(windows)]
use std::ffi::CString;
#[cfg(unix)]
use std::io::Write;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};
#[cfg(windows)]
use winapi::{
    shared::minwindef::FALSE,
    um::{
        handleapi::CloseHandle,
        synchapi::{OpenEventA, SetEvent},
        winnt::EVENT_MODIFY_STATE,
    },
};

use super::*;
use crate::{EndedOutcome, Trigger};

/// How long any one step may take before a test counts it as stuck.
const STEP: Duration = Duration::from_secs(10);

/// How long a test watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(300);

/// Short enough that a runtime that's gone is seen as gone within a test.
const TEST_TIMING: Timing = Timing {
    advertise: Duration::from_secs(5),
    reply: Duration::from_secs(5),
    reconnect: Duration::from_millis(300),
};

const RESUME_RUNTIME: u8 = 0x01;
const PROCESS_INFO2: u8 = 0x04;
const PROCESS_INFO3: u8 = 0x08;

/// A port being served, and what it reported so far.
struct Port {
    address: String,
    /// The port's `report` --each report--> the test.
    report_rx: mpsc::UnboundedReceiver<Report>,
    _serve: JoinHandle<()>,
}

impl Port {
    fn start() -> Self {
        let port = DiagnosticPort::create().expect("the port is created");
        let address = port.address().to_owned();
        let (report_tx, report_rx) = mpsc::unbounded_channel();
        let serve = tokio::spawn(port.serve_with(
            TEST_TIMING,
            Arc::new(move |report| {
                let _ = report_tx.send(report);
            }),
        ));
        Self {
            address,
            report_rx,
            _serve: serve,
        }
    }

    async fn next_report(&mut self) -> Report {
        tokio::time::timeout(STEP, self.report_rx.recv())
            .await
            .expect("the port reported in time")
            .expect("the port still serves")
    }

    async fn next_held(&mut self) -> Announcement {
        match self.next_report().await {
            Report::Held(announcement) => announcement,
            other => panic!("expected a hold, got {other:?}"),
        }
    }

    /// Asserts nothing is reported for a while.
    async fn assert_quiet(&mut self, why: &str) {
        if let Ok(Some(report)) = tokio::time::timeout(QUIET, self.report_rx.recv()).await {
            panic!("{why}, but the port reported {report:?}");
        }
    }
}

/// How a [`FakeRuntime`] answers ProcessInfo3.
#[derive(Clone, Copy)]
enum Info {
    /// The recorded reply of a real .NET 8 runtime, whose entry assembly is `FakeUp`.
    Recorded,
    /// A reply naming this entry assembly.
    EntryAssembly(&'static str),
    /// An error, as a runtime older than .NET 8 answers; ProcessInfo2 then gets the recorded reply.
    OnlyProcessInfo2,
}

/// What a [`FakeRuntime`] does once its ResumeRuntime is answered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnceResumed {
    /// It connects again, as a running program's runtime does.
    Reconnects,
    /// It ends before it connects again, as a short program does.
    Exits,
}

/// A stand-in for a paused .NET runtime.
#[derive(Clone)]
struct FakeRuntime {
    address: String,
    cookie: Uuid,
    /// The pid it advertises, normally its own.
    advertised_pid: u32,
    info: Info,
    once_resumed: OnceResumed,
    /// The command ids it got, in order.
    commands: Arc<Mutex<Vec<u8>>>,
}

impl FakeRuntime {
    fn new(port: &Port, info: Info) -> Self {
        Self {
            address: port.address.clone(),
            cookie: Uuid::new_v4(),
            advertised_pid: std::process::id(),
            info,
            once_resumed: OnceResumed::Reconnects,
            commands: Arc::default(),
        }
    }

    fn commands(&self) -> Vec<u8> {
        self.commands.lock().expect("commands").clone()
    }

    /// Connects and sends its advertise, as a runtime does on every connection.
    async fn advertise(&self) -> Client {
        let mut client = connect(&self.address).await;
        let mut frame = Vec::with_capacity(ADVERTISE_SIZE);
        frame.extend_from_slice(b"ADVR_V1\0");
        frame.extend_from_slice(&self.cookie.to_bytes_le());
        frame.extend_from_slice(&u64::from(self.advertised_pid).to_le_bytes());
        frame.extend_from_slice(&[0, 0]);
        client.write_all(&frame).await.expect("advertise");
        client
    }

    /// Reads the one command a connection carries, or `None` when the port closed it first.
    async fn read_command(&self, client: &mut Client) -> Option<u8> {
        let mut header = [0; 20];
        client.read_exact(&mut header).await.ok()?;
        let command = *header.get(17)?;
        self.commands.lock().expect("commands").push(command);
        Some(command)
    }

    fn reply_to(&self, command: u8) -> Vec<u8> {
        match (command, self.info) {
            (RESUME_RUNTIME, _) => recorded(include_str!("testdata/resume.hex")),
            (PROCESS_INFO3, Info::Recorded) => recorded(include_str!("testdata/processinfo3.hex")),
            (PROCESS_INFO3, Info::EntryAssembly(name)) => process_info3_reply(name),
            (PROCESS_INFO2, _) => recorded(include_str!("testdata/processinfo2.hex")),
            // An unknown command, as an older runtime answers ProcessInfo3.
            _ => error_reply(0x8013_1384),
        }
    }

    /// Runs as a runtime does: a command per connection, then a new connection, for good.
    fn spawn(&self) -> JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            loop {
                let mut client = runtime.advertise().await;
                if let Some(command) = runtime.read_command(&mut client).await {
                    let reply = runtime.reply_to(command);
                    client.write_all(&reply).await.expect("reply");
                    if command == RESUME_RUNTIME && runtime.once_resumed == OnceResumed::Exits {
                        return;
                    }
                }
            }
        })
    }
}

#[cfg(windows)]
type Client = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(windows)]
async fn connect(address: &str) -> Client {
    let path = format!(r"\\.\pipe\{address}");
    tokio::time::timeout(STEP, async {
        loop {
            // Busy while the port creates its next instance; the runtime retries the same way.
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&path) {
                Ok(client) => break client,
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("the port takes connections")
}

#[cfg(unix)]
type Client = tokio::net::UnixStream;

#[cfg(unix)]
async fn connect(address: &str) -> Client {
    Client::connect(address)
        .await
        .expect("the port takes connections")
}

/// What an IDE does once its debugger is ready: set the announced event.
#[cfg(windows)]
fn release_like_an_ide(release: &str) {
    let name = CString::new(release).expect("name");
    let event = unsafe { OpenEventA(EVENT_MODIFY_STATE, FALSE, name.as_ptr()) };
    assert!(!event.is_null(), "the IDE opens the announced event");
    unsafe {
        assert_ne!(SetEvent(event), 0, "the IDE sets it");
        CloseHandle(event);
    }
}

/// What an IDE does once its debugger is ready: write a byte to the announced FIFO.
#[cfg(unix)]
fn release_like_an_ide(release: &str) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(release)
        .expect("the gate keeps its FIFO open for reading")
        .write_all(b"x")
        .expect("the release accepts a byte");
}

fn recorded(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2).expect("whole bytes"), 16).expect("hex"))
        .collect()
}

fn frame(payload: &[u8], id: u8) -> Vec<u8> {
    let mut frame = b"DOTNET_IPC_V1\0".to_vec();
    let size = u16::try_from(20 + payload.len()).expect("a small frame");
    frame.extend_from_slice(&size.to_le_bytes());
    frame.extend_from_slice(&[0xFF, id, 0, 0]);
    frame.extend_from_slice(payload);
    frame
}

fn error_reply(hresult: u32) -> Vec<u8> {
    frame(&hresult.to_le_bytes(), 0xFF)
}

/// A ProcessInfo3 reply, laid out as the runtime lays it out, naming `entry_assembly`.
fn process_info3_reply(entry_assembly: &str) -> Vec<u8> {
    let string = |payload: &mut Vec<u8>, text: &str| {
        let units = text.encode_utf16().chain([0]).collect::<Vec<_>>();
        payload.extend_from_slice(&u32::try_from(units.len()).expect("short").to_le_bytes());
        units
            .iter()
            .for_each(|unit| payload.extend_from_slice(&unit.to_le_bytes()));
    };
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&u64::from(std::process::id()).to_le_bytes());
    payload.extend_from_slice(&[0; 16]);
    for text in [
        "\"dotnet\" run",
        "Windows",
        "x64",
        entry_assembly,
        "10.0.0",
        "win-x64",
    ] {
        string(&mut payload, text);
    }
    frame(&payload, 0x00)
}

/// A paused runtime is announced once, with what it said about itself, however often it
/// reconnects, and it's resumed only once the IDE releases it.
#[tokio::test]
async fn a_paused_runtime_is_resumed_only_after_its_release() {
    let mut port = Port::start();
    let runtime = FakeRuntime::new(&port, Info::Recorded);
    let _running = runtime.spawn();

    let held = port.next_held().await;
    assert_eq!(held.trigger, Trigger::Runtime);
    assert_eq!(held.pid, std::process::id());
    assert_eq!(held.runtime.entry_assembly.as_deref(), Some("FakeUp"));
    assert_eq!(
        held.runtime.runtime.as_deref(),
        Some(runtime.cookie.to_string().as_str())
    );
    port.assert_quiet("nobody released it").await;
    assert_eq!(
        runtime.commands(),
        [PROCESS_INFO3],
        "a held runtime is sent nothing until its release"
    );

    release_like_an_ide(&held.release);

    match port.next_report().await {
        Report::Ended(ended) => {
            assert_eq!(ended.outcome, EndedOutcome::Resumed);
            assert_eq!(ended.release, held.release, "the end names its hold");
        }
        other => panic!("expected the hold's end, got {other:?}"),
    }
    assert_eq!(runtime.commands(), [PROCESS_INFO3, RESUME_RUNTIME]);
    port.assert_quiet("a resumed runtime is never held again, though it reconnects")
        .await;
}

/// The runtime reconnects before its first reply is even read. Nothing may be decided on the new
/// connection until that reply is: deciding on an empty answer resumed an app it should have held.
#[tokio::test]
async fn nothing_is_decided_on_a_reconnect_before_the_first_reply_is_read() {
    let mut port = Port::start();
    let runtime = FakeRuntime::new(&port, Info::Recorded);

    let mut first = runtime.advertise().await;
    assert_eq!(runtime.read_command(&mut first).await, Some(PROCESS_INFO3));
    // The reconnect, while the first reply is still unsent.
    let mut second = runtime.advertise().await;
    let early = tokio::time::timeout(QUIET, runtime.read_command(&mut second)).await;
    assert!(
        early.is_err(),
        "the second connection got a command before the first reply was read: {early:?}"
    );
    port.assert_quiet("nothing is known about the runtime yet")
        .await;

    first
        .write_all(&runtime.reply_to(PROCESS_INFO3))
        .await
        .expect("the late reply");
    drop(first);

    let held = port.next_held().await;
    assert_eq!(
        held.runtime.entry_assembly.as_deref(),
        Some("FakeUp"),
        "decided from the reply, once it was read"
    );
    let resumed = tokio::time::timeout(QUIET, runtime.read_command(&mut second)).await;
    assert!(
        resumed.is_err(),
        "the app is held, so its connection gets no resume: {resumed:?}"
    );
}

/// The SDK's own processes, here the SDK under `dotnet run`, are resumed at once, and the IDE
/// hears nothing of them.
#[tokio::test]
async fn sdk_tooling_is_resumed_with_no_announcement() {
    let mut port = Port::start();
    let runtime = FakeRuntime::new(&port, Info::EntryAssembly("dotnet"));
    let _running = runtime.spawn();

    match port.next_report().await {
        Report::Skipped {
            pid,
            entry_assembly,
        } => {
            assert_eq!(pid, std::process::id());
            assert_eq!(entry_assembly, "dotnet");
        }
        other => panic!("expected the SDK to be skipped, got {other:?}"),
    }
    assert_eq!(runtime.commands(), [PROCESS_INFO3, RESUME_RUNTIME]);
    port.assert_quiet("the SDK is never held").await;
}

/// A process can't get another one held, and attached, by advertising its pid.
#[tokio::test]
async fn a_peer_that_isnt_the_advertised_process_is_refused() {
    let mut port = Port::start();
    let mut runtime = FakeRuntime::new(&port, Info::Recorded);
    // Not this process: the IDE's own, say.
    runtime.advertised_pid = std::process::id() + 1;

    let mut client = runtime.advertise().await;

    assert!(
        matches!(port.next_report().await, Report::Refused(_)),
        "the pids differ, so the connection is refused"
    );
    assert_eq!(
        runtime.read_command(&mut client).await,
        None,
        "a refused connection is closed, and carries no command"
    );
}

/// A held runtime whose process ends, here by dropping its connections for good, ends its hold:
/// the IDE must not wait to release it.
#[tokio::test]
async fn a_runtime_that_exits_while_held_ends_its_hold() {
    let mut port = Port::start();
    let runtime = FakeRuntime::new(&port, Info::Recorded);
    let running = runtime.spawn();
    let held = port.next_held().await;

    running.abort();

    match port.next_report().await {
        Report::Ended(ended) => {
            assert_eq!(ended.outcome, EndedOutcome::Exited);
            assert_eq!(ended.release, held.release);
        }
        other => panic!("expected the hold's end, got {other:?}"),
    }
    assert_eq!(
        runtime.commands(),
        [PROCESS_INFO3],
        "a runtime that's gone is never resumed"
    );
}

/// A runtime older than .NET 8 answers ProcessInfo3 with an error. ProcessInfo2, on the next
/// connection, still names its entry assembly.
#[tokio::test]
async fn a_runtime_without_process_info3_is_asked_process_info2() {
    let mut port = Port::start();
    let runtime = FakeRuntime::new(&port, Info::OnlyProcessInfo2);
    let _running = runtime.spawn();

    let held = port.next_held().await;

    assert_eq!(held.runtime.entry_assembly.as_deref(), Some("FakeUp"));
    assert_eq!(
        held.runtime.rid, None,
        "only ProcessInfo3 has a runtime identifier"
    );
    assert_eq!(runtime.commands(), [PROCESS_INFO3, PROCESS_INFO2]);
}

/// Two runtimes held at once, as an app and a child app: each has its own release, and
/// releasing one leaves the other held.
#[tokio::test]
async fn two_runtimes_are_held_and_released_on_their_own() {
    let mut port = Port::start();
    let first = FakeRuntime::new(&port, Info::Recorded);
    let second = FakeRuntime::new(&port, Info::Recorded);
    let _first_running = first.spawn();
    let first_held = port.next_held().await;
    let _second_running = second.spawn();
    let second_held = port.next_held().await;
    assert_ne!(first_held.release, second_held.release);

    release_like_an_ide(&second_held.release);

    match port.next_report().await {
        Report::Ended(ended) => assert_eq!(ended.release, second_held.release),
        other => panic!("expected the second hold's end, got {other:?}"),
    }
    assert_eq!(second.commands(), [PROCESS_INFO3, RESUME_RUNTIME]);
    assert_eq!(
        first.commands(),
        [PROCESS_INFO3],
        "the first runtime is still held"
    );
}

/// What [`real_runtime`] built.
struct RealApp {
    /// The temporary directory everything was built in, removed with this.
    _scratch: tempfile::TempDir,
    /// The fixture's project, built in place, so `dotnet run --no-build` finds its output.
    project: std::path::PathBuf,
    /// Its IL `.dll`, which `dotnet` runs.
    dll: std::path::PathBuf,
}

/// Builds `testdata/hello` offline with the `dotnet` on PATH, or says why it can't and returns
/// `None`. Set `MIRRORD_REQUIRE_DOTNET=1` to make a missing SDK a failure instead.
fn real_runtime() -> Option<RealApp> {
    let has_sdk = mirrord_command::resolve_command("dotnet")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !has_sdk {
        assert!(
            std::env::var_os("MIRRORD_REQUIRE_DOTNET").is_none(),
            "MIRRORD_REQUIRE_DOTNET is set, but there's no .NET SDK on PATH"
        );
        eprintln!("skipped: no .NET SDK on PATH to build a real runtime's app with");
        return None;
    }

    let scratch = tempfile::tempdir().expect("a scratch directory");
    let project = scratch.path().join("project");
    let feed = scratch.path().join("empty-feed");
    for directory in [&project, &feed] {
        std::fs::create_dir_all(directory).expect("the scratch directory is writable");
    }
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/testdata/hello");
    for file in [
        "Hello.csproj",
        "Program.cs",
        "Directory.Build.props",
        "Directory.Build.targets",
    ] {
        std::fs::copy(fixture.join(file), project.join(file)).expect("the fixture's files exist");
    }

    // Offline, and without build servers, so nothing downloads and no compiler outlives the test.
    let built = mirrord_command::resolve_command("dotnet")
        .args([
            "build",
            "-c",
            "Release",
            "--nologo",
            "--disable-build-servers",
        ])
        .arg("-p:UseSharedCompilation=false")
        .arg("--source")
        .arg(&feed)
        .arg(project.join("Hello.csproj"))
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .env("MSBUILDDISABLENODEREUSE", "1")
        .output()
        .expect("dotnet runs");
    assert!(
        built.status.success(),
        "the fixture app must build:\n{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    // `bin/Release/<the SDK's framework>/`, where `dotnet run --no-build` looks for it too.
    let dll = std::fs::read_dir(project.join("bin/Release"))
        .expect("the build made its output directory")
        .flatten()
        .map(|framework| framework.path().join("Hello.dll"))
        .find(|dll| dll.is_file())
        .expect("the build made the app's dll");
    Some(RealApp {
        dll,
        project,
        _scratch: scratch,
    })
}

/// A real .NET runtime, `dotnet Hello.dll` with the port in its `DOTNET_DiagnosticPorts`, pauses
/// before its first line, is announced under its own pid with its entry assembly, and runs only
/// once the IDE releases it.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_runtime_pauses_on_the_port_until_its_release() {
    let Some(app) = tokio::task::spawn_blocking(real_runtime)
        .await
        .expect("the build ran")
    else {
        return;
    };
    let port = DiagnosticPort::create().expect("the port is created");
    let ports = format!("{},connect,suspend", port.address());
    let (report_tx, mut report_rx) = mpsc::unbounded_channel();
    let _serve = tokio::spawn(port.serve(move |report| {
        let _ = report_tx.send(report);
    }));

    let mut child = mirrord_command::resolve_tokio_command("dotnet")
        .arg(&app.dll)
        .env("DOTNET_DiagnosticPorts", ports)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the app starts");
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().expect("piped")).lines();

    let held = match tokio::time::timeout(STEP, report_rx.recv()).await {
        Ok(Some(Report::Held(held))) => held,
        other => panic!("expected the runtime's hold, got {other:?}"),
    };
    assert_eq!(Some(held.pid), child.id(), "held under the app's own pid");
    assert_eq!(held.runtime.entry_assembly.as_deref(), Some("Hello"));
    assert!(
        held.process
            .image
            .as_deref()
            .is_some_and(|image| std::path::Path::new(image).is_absolute()),
        "the IDE gets the image's whole path: {:?}",
        held.process.image
    );
    let early = tokio::time::timeout(QUIET, stdout.next_line()).await;
    assert!(early.is_err(), "the app ran before its release: {early:?}");

    release_like_an_ide(&held.release);

    match tokio::time::timeout(STEP, report_rx.recv()).await {
        Ok(Some(Report::Ended(ended))) => assert_eq!(ended.outcome, EndedOutcome::Resumed),
        other => panic!("expected the hold's end, got {other:?}"),
    }
    let line = tokio::time::timeout(STEP, stdout.next_line())
        .await
        .expect("the app runs once released")
        .expect("its output is readable");
    assert_eq!(line.as_deref(), Some("hello from a paused runtime"));
    let status = tokio::time::timeout(STEP, child.wait())
        .await
        .expect("the app ends")
        .expect("its status is readable");
    assert!(status.success(), "the app ran to its end: {status}");
}

/// `dotnet run --no-build`, the shape of a service that runs its project: the SDK pauses first and
/// is resumed without a word, and then the app it starts pauses, and is held.
#[tokio::test(flavor = "multi_thread")]
async fn under_dotnet_run_the_sdk_is_skipped_and_the_app_is_held() {
    let Some(app) = tokio::task::spawn_blocking(real_runtime)
        .await
        .expect("the build ran")
    else {
        return;
    };
    let port = DiagnosticPort::create().expect("the port is created");
    let ports = format!("{},connect,suspend", port.address());
    let (report_tx, mut report_rx) = mpsc::unbounded_channel();
    let _serve = tokio::spawn(port.serve(move |report| {
        let _ = report_tx.send(report);
    }));

    let mut child = mirrord_command::resolve_tokio_command("dotnet")
        .args([
            "run",
            "--no-build",
            "-c",
            "Release",
            "--no-launch-profile",
            "--project",
        ])
        .arg(&app.project)
        .env("DOTNET_DiagnosticPorts", ports)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the SDK starts");
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().expect("piped")).lines();

    match tokio::time::timeout(STEP, report_rx.recv()).await {
        Ok(Some(Report::Skipped {
            pid,
            entry_assembly,
        })) => {
            assert_eq!(
                Some(pid),
                child.id(),
                "the SDK is the process this test started"
            );
            assert_eq!(entry_assembly, "dotnet");
        }
        other => panic!("expected the SDK to be skipped, got {other:?}"),
    }
    let held = match tokio::time::timeout(STEP, report_rx.recv()).await {
        Ok(Some(Report::Held(held))) => held,
        other => panic!("expected the app's hold, got {other:?}"),
    };
    assert_ne!(Some(held.pid), child.id(), "the app is the SDK's child");
    assert_eq!(held.runtime.entry_assembly.as_deref(), Some("Hello"));

    release_like_an_ide(&held.release);

    let line = tokio::time::timeout(STEP, stdout.next_line())
        .await
        .expect("the app runs once released")
        .expect("its output is readable");
    assert_eq!(line.as_deref(), Some("hello from a paused runtime"));
    let status = tokio::time::timeout(STEP, child.wait())
        .await
        .expect("the SDK ends with its app")
        .expect("its status is readable");
    assert!(status.success(), "the app ran to its end: {status}");
}

/// A program that ends right after its resume never connects again. Its hold still ended in a
/// resume, which the IDE must hear, not an exit while held.
#[tokio::test]
async fn a_runtime_that_ends_right_after_its_resume_was_still_resumed() {
    let mut port = Port::start();
    let mut runtime = FakeRuntime::new(&port, Info::Recorded);
    runtime.once_resumed = OnceResumed::Exits;
    let _running = runtime.spawn();
    let held = port.next_held().await;

    release_like_an_ide(&held.release);

    match port.next_report().await {
        Report::Ended(ended) => assert_eq!(
            ended.outcome,
            EndedOutcome::Resumed,
            "the runtime answered ResumeRuntime, so it went on"
        ),
        other => panic!("expected the hold's end, got {other:?}"),
    }
}

/// A resumed runtime whose task already ended, because it stopped reconnecting for longer than
/// the port waits, and then reconnects late, is not held or announced again: its first hold
/// already ran, and the IDE already let it go.
#[tokio::test]
async fn a_resumed_runtime_that_reconnects_late_is_not_held_again() {
    let mut port = Port::start();
    let mut runtime = FakeRuntime::new(&port, Info::Recorded);
    runtime.once_resumed = OnceResumed::Exits;
    let running = runtime.spawn();
    let held = port.next_held().await;
    release_like_an_ide(&held.release);
    match port.next_report().await {
        Report::Ended(ended) => assert_eq!(ended.outcome, EndedOutcome::Resumed),
        other => panic!("expected the hold's end, got {other:?}"),
    }
    running.await.expect("the runtime ends once resumed");
    // Past the reconnect wait, so its task has ended and left only its record.
    tokio::time::sleep(TEST_TIMING.reconnect * 3).await;

    let mut late = runtime.advertise().await;

    let command = tokio::time::timeout(QUIET, runtime.read_command(&mut late)).await;
    assert!(
        command.is_err(),
        "a late connection of a resumed runtime is only kept idle: {command:?}"
    );
    port.assert_quiet("a resumed runtime is never held again")
        .await;
    assert_eq!(runtime.commands(), [PROCESS_INFO3, RESUME_RUNTIME]);
}

//! Supervision under `--output json`: the events a run's services and their ends become, through
//! real processes and pipes, on every platform.
//!
//! Every service is this test binary, re-executed into [`scripted_session`], which plays a script
//! from `testdata/output/`. libtest prints a line or two of its own first, so the tests look at the
//! events they're about rather than at every line.

use std::process::Stdio;

use mirrord_command::resolve_tokio_command;
use rstest::rstest;
use serde_json::Value;

use super::{
    test_support::{TEST_GRACE, TIMEOUT},
    *,
};
use crate::output::Printed;

/// The script a [`scripted_session`] plays.
const SCRIPT_ENV: &str = "MIRRORD_UP_TEST_SCRIPT";

/// The error a failing session's `mirrord exec` printed, as a real `mirrord ls` run printed it.
const SESSION_ERROR: &str = include_str!("../testdata/output/ls-error.txt");

/// What a user's Ctrl-C delivers.
#[cfg(windows)]
const STOP: ShutdownSignal = ShutdownSignal::CtrlC;
#[cfg(unix)]
const STOP: ShutdownSignal = ShutdownSignal::Interrupt;

/// Plays the script in [`SCRIPT_ENV`], one step a line:
///
/// - `stdout <line>` and `stderr <line>`: writes the line to that stream.
/// - `exit <code>`: exits with the code, before libtest prints anything more.
/// - `wait`: keeps running, until the test stops it.
#[test]
#[ignore = "subprocess entry point for the JSON supervision tests"]
fn scripted_session() {
    let Ok(script) = std::env::var(SCRIPT_ENV) else {
        return;
    };
    for step in script.lines() {
        let (verb, rest) = step.split_once(' ').unwrap_or((step, ""));
        match verb {
            "stdout" => println!("{rest}"),
            "stderr" => eprintln!("{rest}"),
            "exit" => std::process::exit(rest.parse().expect("`exit` takes a code")),
            "wait" => std::thread::sleep(Duration::from_secs(60)),
            other => panic!("unknown script step {other}"),
        }
    }
    std::process::exit(0);
}

/// This test binary, re-executed to play `script`.
fn scripted(script: &str) -> Command {
    let mut command = resolve_tokio_command(std::env::current_exe().expect("tests know their exe"));
    command
        .args([
            "--exact",
            "process::event_tests::scripted_session",
            "--ignored",
            "--nocapture",
            "--quiet",
        ])
        .env(SCRIPT_ENV, script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// A service called `api` whose session plays `script`.
fn service(script: &str) -> ServiceCommands {
    ServiceCommands {
        name: Arc::from("api"),
        debugged: false,
        pre: None,
        dotnet_port: None,
        session: scripted(script),
    }
}

/// Every event printed so far.
fn events(printed: &Mutex<Vec<Printed>>) -> Vec<Value> {
    printed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|printed| {
            assert_eq!(printed.stream, Stream::Stdout, "every event goes to stdout");
            serde_json::from_str(&printed.line).expect("every line is one JSON event")
        })
        .collect()
}

/// The events of `kind`.
fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == kind)
        .collect()
}

/// Supervises `commands` under `--output json` until a session exits or `shutdown` resolves, and
/// returns how the run ended and every event it printed.
async fn supervise_json(
    commands: Vec<ServiceCommands>,
    printer: Printer,
    printed: &Mutex<Vec<Printed>>,
    shutdown: impl Future<Output = io::Result<ShutdownSignal>>,
    no_runtime_paused: Duration,
) -> (Result<(), UpError>, Vec<Value>) {
    let (result, _) = tokio::time::timeout(
        TIMEOUT,
        supervise_with_second_signal(
            commands,
            ReadyTracker::default(),
            printer,
            shutdown,
            std::future::pending(),
            Timing {
                grace: TEST_GRACE,
                no_runtime_paused,
            },
        ),
    )
    .await
    .expect("the run ended")
    .expect("supervision itself didn't fail");
    (result, events(printed))
}

/// How a session ended, as its `exited` event says.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// `failed_to_start`, with the code.
    FailedToStart(i64),
    /// `crashed`, with the code.
    Crashed(i64),
    /// `succeeded`.
    Succeeded,
}

/// A session's end is one `exited` event, the run's last, saying why it ended and with which
/// code, after the session's own last line.
#[rstest]
// It failed before its program ran: no `Ready!`.
#[case::failed_to_start(
    include_str!("../testdata/output/script-failed-to-start.txt"),
    Ended::FailedToStart(1)
)]
// Its program ran, said `serving`, and exited with 3.
#[case::crashed(include_str!("../testdata/output/script-crash.txt"), Ended::Crashed(3))]
// Its program ran and exited with 0.
#[case::succeeded(include_str!("../testdata/output/script-exit.txt"), Ended::Succeeded)]
#[tokio::test]
async fn a_session_end_is_its_exited_event(#[case] script: &str, #[case] expected: Ended) {
    let (printer, printed) = Printer::captured(OutputFormat::Json);
    let (result, events) = supervise_json(
        vec![service(script)],
        printer,
        &printed,
        std::future::pending(),
        NO_RUNTIME_PAUSED_AFTER,
    )
    .await;

    let exited = of_type(&events, "exited");
    let [exited] = exited.as_slice() else {
        panic!("one session ended, so one `exited`: {events:#?}");
    };
    assert_eq!(events.last(), Some(*exited), "`exited` ends the run");
    assert_eq!(exited["service"], "api", "`exited` names the service");
    let ended = match (exited["reason"].as_str(), exited["code"].as_i64()) {
        (Some("failed_to_start"), Some(code)) => Ended::FailedToStart(code),
        (Some("crashed"), Some(code)) => Ended::Crashed(code),
        (Some("succeeded"), Some(0)) => Ended::Succeeded,
        _ => panic!("unexpected end {exited}"),
    };
    assert_eq!(ended, expected, "`exited` says why and with which code");
    match expected {
        Ended::FailedToStart(_) => assert!(
            matches!(result, Err(UpError::ServiceFailedToStart { .. })),
            "{result:?}"
        ),
        Ended::Crashed(_) => assert!(
            matches!(result, Err(UpError::ServiceCrashed { .. })),
            "{result:?}"
        ),
        Ended::Succeeded => assert!(result.is_ok(), "{result:?}"),
    }

    // `exited` is last, so the session's last line came before it, as long as it came at all.
    let last_line = script
        .lines()
        .filter_map(|step| step.strip_prefix("stdout "))
        .next_back()
        .expect("each script prints");
    let as_progress = serde_json::from_str::<Value>(last_line).ok();
    assert!(
        events
            .iter()
            .any(|event| event["line"] == last_line
                || Some(&event["progress"]) == as_progress.as_ref()),
        "the session's last stdout line {last_line:?} comes before `exited`: {events:#?}"
    );
}

/// A session whose `mirrord exec` failed with an error report carries it on `exited`, as it was
/// printed, and not as output. The lines before it are still output.
#[tokio::test]
async fn a_session_error_goes_on_exited_not_output() {
    let (printer, printed) = Printer::captured(OutputFormat::Json);
    let (_, events) = supervise_json(
        vec![service(include_str!(
            "../testdata/output/script-failed-to-start.txt"
        ))],
        printer,
        &printed,
        std::future::pending(),
        NO_RUNTIME_PAUSED_AFTER,
    )
    .await;

    let report: Value = serde_json::from_str(
        SESSION_ERROR
            .trim_end()
            .strip_prefix("Error: ")
            .expect("the fixture is an error line"),
    )
    .expect("the fixture's report is JSON");
    let exited = of_type(&events, "exited");
    assert_eq!(
        exited.first().map(|exited| &exited["error"]),
        Some(&report),
        "`exited` carries the session's report as it was printed"
    );
    let stderr = of_type(&events, "output")
        .into_iter()
        .filter(|output| output["stream"] == "stderr")
        .map(|output| output["line"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        stderr,
        ["Kubernetes environment:"],
        "only the line before the report is output"
    );
}

/// A failed pre-command of the last service left is said as an event, though without the flag
/// the run's error alone says it. Its output is marked as the pre-command's.
#[tokio::test]
async fn the_last_failed_pre_command_is_an_event() {
    let mut api = service(include_str!("../testdata/output/script-exit.txt"));
    api.pre = Some(scripted(include_str!(
        "../testdata/output/script-pre-command-fails.txt"
    )));
    let (printer, printed) = Printer::captured(OutputFormat::Json);
    let (result, events) = supervise_json(
        vec![api],
        printer,
        &printed,
        std::future::pending(),
        NO_RUNTIME_PAUSED_AFTER,
    )
    .await;

    assert!(
        matches!(result, Err(UpError::PreCommandFailed { .. })),
        "{result:?}"
    );
    let stages = of_type(&events, "pre_command")
        .into_iter()
        .map(|event| (event["stage"].as_str(), event["code"].as_i64()))
        .collect::<Vec<_>>();
    assert_eq!(
        stages,
        [(Some("started"), None), (Some("failed"), Some(2))],
        "the pre-command started, then failed with its code"
    );
    let build_output = of_type(&events, "output")
        .into_iter()
        .filter(|output| output["line"] == "build failed")
        .collect::<Vec<_>>();
    assert_eq!(
        build_output.first().map(|output| &output["source"]),
        Some(&Value::from("pre_command")),
        "the pre-command's output is marked as its own"
    );
    assert!(
        of_type(&events, "exited").is_empty(),
        "no session ran, so none exited"
    );
}

/// A debugged service whose runtime was to pause, but hasn't a while after its program started,
/// says so once while it runs, so the IDE stops waiting in silence.
#[tokio::test]
async fn a_runtime_that_never_pauses_is_said_while_the_service_runs() {
    let mut api = service(include_str!(
        "../testdata/output/script-runtime-never-pauses.txt"
    ));
    api.debugged = true;
    let (printer, printed) = Printer::captured(OutputFormat::Json);
    // Stops the run once it said so.
    let said = async {
        while of_type(&events(&printed), "debug_no_runtime_paused").is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(STOP)
    };
    let (result, events) = supervise_json(
        vec![api],
        printer,
        &printed,
        said,
        Duration::from_millis(100),
    )
    .await;

    assert!(result.is_ok(), "{result:?}");
    let said = of_type(&events, "debug_no_runtime_paused");
    let [said] = said.as_slice() else {
        panic!("said once: {events:#?}");
    };
    assert_eq!(
        said["service_state"], "running",
        "said while the service still runs"
    );
    let ready_at = events
        .iter()
        .position(|event| event["type"] == "ready")
        .expect("the program started");
    let said_at = events
        .iter()
        .position(|event| event == *said)
        .expect("it was said");
    assert!(ready_at < said_at, "said only after the program started");
}

/// A session's error report that no `exited` takes, since `up` was stopped first, is still
/// printed, as the stderr line it was, once the session is gone.
#[tokio::test]
async fn a_report_no_exited_takes_is_printed_as_output() {
    let (printer, printed) = Printer::captured(OutputFormat::Json);
    // Stops the run once the session said it reported.
    let reported = async {
        while of_type(&events(&printed), "output")
            .iter()
            .all(|output| output["line"] != "reported")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(STOP)
    };
    let (result, events) = supervise_json(
        vec![service(include_str!(
            "../testdata/output/script-error-then-wait.txt"
        ))],
        printer,
        &printed,
        reported,
        NO_RUNTIME_PAUSED_AFTER,
    )
    .await;

    assert!(result.is_ok(), "{result:?}");
    assert!(
        of_type(&events, "exited").is_empty(),
        "`up` was stopped, so no session exited on its own"
    );
    let stderr = of_type(&events, "output")
        .into_iter()
        .filter(|output| output["stream"] == "stderr")
        .map(|output| output["line"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        stderr,
        [SESSION_ERROR.trim_end()],
        "the report is printed as the line it was"
    );
}

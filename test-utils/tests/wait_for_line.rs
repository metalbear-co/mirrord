#![cfg(unix)]

use std::{collections::HashMap, panic::AssertUnwindSafe, time::Duration};

use futures::FutureExt;
use mirrord_test_utils::TestProcess;

/// Spawns `script` under `sh`, capturing its output.
async fn spawn(script: &str) -> TestProcess {
    TestProcess::start_process(
        "sh".to_owned(),
        vec!["-c".to_owned(), script.to_owned()],
        HashMap::new(),
    )
    .await
}

/// Runs `wait_for_line`, reporting whether it gave up, so its panic does not fail the test.
async fn wait_panics(process: TestProcess, timeout: Duration, line: &str) -> bool {
    AssertUnwindSafe(process.wait_for_line(timeout, line))
        .catch_unwind()
        .await
        .is_err()
}

#[tokio::test]
async fn survives_a_startup_longer_than_the_silence_budget() {
    let process =
        spawn("for _ in 1 2 3 4 5 6; do echo tick >&2; sleep 0.2; done; echo READY >&2; sleep 30")
            .await;

    let started = std::time::Instant::now();
    process
        .wait_for_line(Duration::from_millis(400), "READY")
        .await;

    assert!(
        started.elapsed() > Duration::from_millis(400),
        "test is not exercising an extension: the line arrived within the silence budget"
    );
}

#[tokio::test]
async fn gives_up_on_a_silent_process() {
    let process = spawn("sleep 30").await;

    let started = std::time::Instant::now();
    assert!(wait_panics(process, Duration::from_millis(500), "READY").await);

    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "silent process was not failed on the silence budget, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn gives_up_on_a_chatty_process_that_never_arrives() {
    let process = spawn("while true; do echo noise >&2; sleep 0.05; done").await;

    let started = std::time::Instant::now();
    assert!(wait_panics(process, Duration::from_millis(300), "READY").await);

    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1500) && elapsed < Duration::from_millis(4000),
        "expected the hard deadline to stop it near 1.5s, took {elapsed:?}"
    );
}

#[tokio::test]
async fn gives_up_once_the_stream_closes() {
    let process = spawn("echo oops >&2; exit 1").await;

    let started = std::time::Instant::now();
    assert!(wait_panics(process, Duration::from_secs(30), "READY").await);

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "waited on an exited process instead of failing fast, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn accepts_a_line_written_just_before_the_stream_closed() {
    let process = spawn("echo READY >&2; exit 0").await;

    process
        .wait_for_line(Duration::from_secs(30), "READY")
        .await;
}

//! Helpers shared by the Unix and Windows supervision tests.

use std::{ops::Not, path::Path, time::Duration};

use mirrord_progress::messages::SESSION_READY_MESSAGE;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Child,
};

/// Grace period the tests give services before escalating, short so that escalation tests stay
/// fast. Windows teardown has no graceful phase and ignores it.
pub(super) const TEST_GRACE: Duration = Duration::from_millis(200);

/// How long any one step of a test may take before the test counts it as stuck.
pub(super) const TIMEOUT: Duration = Duration::from_secs(10);

/// Waits for a test process to create `path`, its way of saying it got somewhere.
pub(super) async fn wait_for_file(path: &Path) {
    tokio::time::timeout(TIMEOUT, async {
        while path.exists().not() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{} never appeared", path.display()));
}

/// Waits until a re-executed supervisor helper forwards its service's session-ready line, then
/// keeps draining its stdout so that the helper never blocks on a full pipe.
pub(super) async fn wait_for_helper_ready(helper: &mut Child) {
    let mut lines = BufReader::new(helper.stdout.take().unwrap()).lines();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("helper exited before readiness");
            if line.contains(SESSION_READY_MESSAGE) {
                break;
            }
        }
    })
    .await
    .expect("helper never became ready");
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
}

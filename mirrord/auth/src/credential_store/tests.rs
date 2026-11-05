use std::{ops::Not, sync::mpsc, thread, time::Duration};

use fs4::tokio::AsyncFileExt;
use tokio::fs;

use super::lock_exclusive;

/// Two holders of the credential store in one process, on the CLI's single-threaded runtime, take
/// the lock in turn: the second waits, without stopping the first, until the first unlocks, as one
/// service of `mirrord preview sync` waits while another awaits the operator with the lock held.
#[test]
fn a_second_holder_in_the_same_process_gets_the_lock_once_the_first_unlocks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    // The scenario's thread tells the test that it finished.
    let (done_tx, done_rx) = mpsc::channel();

    // A holder that blocked the runtime's only thread would never return, so the scenario runs on
    // a thread of its own, and the test fails when it doesn't finish in time.
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            let first = options.open(&path).await.unwrap();
            let second = options.open(&path).await.unwrap();

            lock_exclusive(&first).await.unwrap();
            let waiter = tokio::spawn(async move {
                lock_exclusive(&second).await.unwrap();
                second.unlock().unwrap();
            });

            // The first holder awaits while it holds the lock, which lets the waiter run.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                waiter.is_finished().not(),
                "the second holder got the lock while the first held it"
            );

            first.unlock().unwrap();
            waiter.await.unwrap();
        });

        let _ = done_tx.send(());
    });

    assert!(
        done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        "the second holder didn't get the lock"
    );
}

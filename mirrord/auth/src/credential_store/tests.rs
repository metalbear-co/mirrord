use std::{sync::mpsc, thread, time::Duration};

use fs4::tokio::AsyncFileExt;
use tokio::fs;

use super::lock_exclusive;

/// Two holders of the credential store in one process, on the CLI's single-threaded runtime, both
/// get the lock: the second waits while the first awaits, as one service of `mirrord preview sync`
/// waits for the operator to sign its certificate while the session of another service starts.
#[test]
fn a_second_holder_in_the_same_process_gets_the_lock_after_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    let (done, finished) = mpsc::channel();

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

            // The first holder awaits while it holds the lock, which lets the waiter start.
            tokio::time::sleep(Duration::from_millis(100)).await;
            first.unlock().unwrap();
            waiter.await.unwrap();
        });

        let _ = done.send(());
    });

    assert!(
        finished.recv_timeout(Duration::from_secs(5)).is_ok(),
        "the second holder didn't get the lock"
    );
}

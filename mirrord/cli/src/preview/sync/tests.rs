use std::{fs, path::Path, sync::Arc, time::Duration};

use futures::{SinkExt, StreamExt};
use mirrord_operator::{client::preview_sync::PreviewSyncConnection, sync::SyncFrame};
use mirrord_progress::ProgressTracker;
use tokio::io::DuplexStream;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{PreviewSync, WATCH_DEBOUNCE};

/// The operator's end of the sync connection, which answers every push as the sidecar would.
struct Operator {
    socket: WebSocketStream<DuplexStream>,
    generation: u64,
}

impl Operator {
    /// Waits up to `timeout` for the next push, applies it, and returns the entries it named.
    /// Fails when a push starts but doesn't end within `timeout`.
    async fn next_push(&mut self, timeout: Duration) -> Option<Vec<u32>> {
        let message = tokio::time::timeout(timeout, self.socket.next())
            .await
            .ok()?;
        let Message::Binary(bytes) = message.unwrap().unwrap() else {
            panic!("the CLI sent a message that isn't a frame");
        };

        let SyncFrame::Begin { push_id, entries } = SyncFrame::decode(&bytes).unwrap() else {
            panic!("a push starts with `Begin`");
        };

        tokio::time::timeout(timeout, async {
            loop {
                let Message::Binary(bytes) = self.socket.next().await.unwrap().unwrap() else {
                    panic!("the CLI sent a message that isn't a frame");
                };

                match SyncFrame::decode(&bytes).unwrap() {
                    SyncFrame::Data { .. } => {}
                    SyncFrame::Commit { .. } => break,
                    frame => panic!("unexpected frame in a push: {frame:?}"),
                }
            }
        })
        .await
        .expect("the push started, but `Commit` didn't arrive");

        self.generation += 1;
        let applied = SyncFrame::Applied {
            push_id,
            generation: self.generation,
        };

        self.socket
            .send(Message::Binary(applied.encode().into()))
            .await
            .unwrap();

        Some(entries)
    }
}

/// How long `--watch` gets to push a change: the debounce, packing and the push.
const PUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the test waits to see that `--watch` pushes nothing: the debounce, and as long again
/// for packing and sending the push.
const QUIET_PERIOD: Duration = WATCH_DEBOUNCE.saturating_mul(2);

/// `--watch` pushes an entry once per change to the files that a push carries, including a change
/// made after the first push but before it started watching, and never for what a push doesn't
/// carry, such as a build's output in an ignored directory.
#[tokio::test]
async fn watch_pushes_each_change_to_the_pushed_files_once() {
    let frontend = tempfile::tempdir().unwrap();
    let backend = tempfile::tempdir().unwrap();
    let write = |dir: &Path, path: &str, contents: &str| {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    // Git's ignore rules only apply inside a repository.
    fs::create_dir(frontend.path().join(".git")).unwrap();
    write(frontend.path(), ".gitignore", "dist/\n");
    write(frontend.path(), "app.js", "v1");
    write(backend.path(), "server.js", "v1");

    let (cli, operator) = tokio::io::duplex(1 << 20);
    let (cli, operator) = tokio::join!(
        WebSocketStream::from_raw_socket(cli, Role::Client, None),
        WebSocketStream::from_raw_socket(operator, Role::Server, None),
    );
    let mut operator = Operator {
        socket: operator,
        generation: 0,
    };
    let locals = Arc::from([frontend.path().to_owned(), backend.path().to_owned()]);

    let (first_push, entries) = tokio::join!(
        PreviewSync::first_push(PreviewSyncConnection::new(cli), locals),
        operator.next_push(PUSH_TIMEOUT),
    );
    let (preview_sync, generation) = first_push.unwrap();
    assert_eq!((generation, entries), (1, Some(vec![0, 1])));

    write(frontend.path(), "app.js", "v2, written before the watch");

    let progress = ProgressTracker::null();
    let edits = async {
        assert_eq!(operator.next_push(PUSH_TIMEOUT).await, Some(vec![0]));

        write(frontend.path(), "app.js", "v3, a longer edit");
        assert_eq!(operator.next_push(PUSH_TIMEOUT).await, Some(vec![0]));
        assert_eq!(
            operator.next_push(QUIET_PERIOD).await,
            None,
            "a push followed although the tree didn't change"
        );

        write(frontend.path(), "dist/bundle.js", "bundle");
        assert_eq!(
            operator.next_push(QUIET_PERIOD).await,
            None,
            "a change to an ignored file started a push"
        );

        write(backend.path(), "server.js", "v2, a longer edit");
        assert_eq!(operator.next_push(PUSH_TIMEOUT).await, Some(vec![1]));
    };

    // `watch` starts watching before it first waits, so polling it first makes it see the edits.
    tokio::select! {
        biased;
        result = preview_sync.watch(&progress) => panic!("`--watch` ended: {result:?}"),
        () = edits => {}
    }
}

use std::{collections::BTreeMap, fs, io::Read, path::Path};

use flate2::read::GzDecoder;
use futures::{SinkExt, StreamExt};
use tokio::io::DuplexStream;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{PreviewSyncConnection, PreviewSyncError, TreeListing, pack_directory};
use crate::sync::{MAX_DATA_CHUNK_BYTES, SidecarState, SyncFrame};

/// What an archive holds at one path.
#[derive(Debug, PartialEq, Eq)]
enum Packed {
    Directory,

    /// A file with its contents and whether its owner may execute it.
    File(String, bool),

    /// A symlink with its target.
    Symlink(String),
}

/// What the archive that `pack_directory` makes of `local` holds, by path.
fn packed(local: &Path) -> BTreeMap<String, Packed> {
    let archive = pack_directory(local).unwrap();
    let mut archive = tar::Archive::new(GzDecoder::new(archive.as_slice()));
    let mut packed = BTreeMap::new();

    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().display().to_string();
        let header = entry.header();

        let item = if header.entry_type().is_dir() {
            Packed::Directory
        } else if header.entry_type().is_symlink() {
            Packed::Symlink(entry.link_name().unwrap().unwrap().display().to_string())
        } else {
            let executable = header.mode().unwrap() & 0o100 != 0;
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            Packed::File(contents, executable)
        };

        packed.insert(path.trim_end_matches('/').to_owned(), item);
    }

    packed
}

/// A push must carry the tree that the developer works on: what git ignores stays on their
/// machine, and what the dev server needs, such as executable scripts and symlinks, keeps its
/// kind.
#[cfg(unix)]
#[test]
fn push_leaves_out_what_git_ignores() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let local = tempfile::tempdir().unwrap();
    let write = |path: &str, contents: &str| {
        let path = local.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    write(".gitignore", "dist/\n*.log\n");
    write("app.js", "app");
    write("run.sh", "#!/bin/sh");
    fs::set_permissions(
        local.path().join("run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write("lib/util.js", "util");
    write("lib/.gitignore", "generated.js\n");
    write("lib/generated.js", "generated");
    write("dist/bundle.js", "bundle");
    write("debug.log", "log");
    // Git's ignore rules only apply inside a repository, and the push leaves out the
    // repository's own directories.
    write(".git/HEAD", "ref: refs/heads/main");
    write(".jj/.gitignore", "/*\n");
    write(".jj/repo/store", "store");
    symlink("app.js", local.path().join("link")).unwrap();

    let file = |contents: &str, executable| Packed::File(contents.to_owned(), executable);
    assert_eq!(
        packed(local.path()),
        BTreeMap::from([
            (".gitignore".to_owned(), file("dist/\n*.log\n", false)),
            ("app.js".to_owned(), file("app", false)),
            ("lib".to_owned(), Packed::Directory),
            ("lib/.gitignore".to_owned(), file("generated.js\n", false)),
            ("lib/util.js".to_owned(), file("util", false)),
            ("link".to_owned(), Packed::Symlink("app.js".to_owned())),
            ("run.sh".to_owned(), file("#!/bin/sh", true)),
        ])
    );
}

/// A service's directory is often a subdirectory of its repository, so its push must leave out
/// what the repository's root `.gitignore` and `.git/info/exclude` exclude. A `.gitignore` above
/// the repository doesn't count, as in git, because it can belong to something else, such as a
/// home directory that ignores everything.
#[test]
fn push_of_a_subdirectory_leaves_out_what_its_repository_ignores() {
    let home = tempfile::tempdir().unwrap();
    let write = |path: &str, contents: &str| {
        let path = home.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    write(".gitignore", "*\n");
    write("repo/.git/HEAD", "ref: refs/heads/main");
    write("repo/.git/info/exclude", "notes.txt\n");
    write("repo/.gitignore", "node_modules/\n.env\n");
    write("repo/frontend/app.js", "app");
    write("repo/frontend/.env", "SECRET=1");
    write("repo/frontend/notes.txt", "notes");
    write("repo/frontend/node_modules/left-pad/index.js", "dependency");

    assert_eq!(
        packed(&home.path().join("repo/frontend")),
        BTreeMap::from([("app.js".to_owned(), Packed::File("app".to_owned(), false))])
    );
}

/// A dev server's socket in the tree must not stop the push of the files around it.
#[cfg(unix)]
#[test]
fn push_skips_sockets() {
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("app.js"), "app").unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(local.path().join("dev.sock")).unwrap();

    assert_eq!(
        packed(local.path()),
        BTreeMap::from([("app.js".to_owned(), Packed::File("app".to_owned(), false))])
    );
}

/// `--watch` pushes the tree again only when its listing changes, so a build that writes into an
/// ignored directory must not change it, and an edit of a pushed file must.
#[test]
fn listing_changes_only_with_the_pushed_tree() {
    let local = tempfile::tempdir().unwrap();
    // Git's ignore rules only apply inside a repository.
    fs::create_dir(local.path().join(".git")).unwrap();
    fs::write(local.path().join(".gitignore"), "dist/\n").unwrap();
    fs::write(local.path().join("app.js"), "v1").unwrap();
    let pushed = TreeListing::new(local.path()).unwrap();

    fs::create_dir(local.path().join("dist")).unwrap();
    fs::write(local.path().join("dist/bundle.js"), "bundle").unwrap();
    assert_eq!(TreeListing::new(local.path()).unwrap(), pushed);

    fs::write(local.path().join("app.js"), "v2, longer").unwrap();
    assert_ne!(TreeListing::new(local.path()).unwrap(), pushed);
}

/// The operator's end of a [`PreviewSyncConnection`], which a test scripts.
struct Operator(WebSocketStream<DuplexStream>);

impl Operator {
    async fn send(&mut self, frame: SyncFrame) {
        self.0
            .send(Message::Binary(frame.encode().into()))
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> SyncFrame {
        match self.0.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => SyncFrame::decode(&bytes).unwrap(),
            message => panic!("the CLI sent {message:?}"),
        }
    }

    /// Receives a push of the entries and returns its id, and what each entry's `Data` frames
    /// carried, in the order they came.
    async fn recv_push(&mut self) -> (u64, Vec<(u32, Vec<u8>)>) {
        let SyncFrame::Begin { push_id, entries } = self.recv().await else {
            panic!("a push starts with `Begin`");
        };

        let mut received: Vec<(u32, Vec<u8>)> = Vec::new();

        loop {
            match self.recv().await {
                SyncFrame::Data { index, chunk } => {
                    assert!(entries.contains(&index), "entry {index} isn't in the push");
                    assert!(chunk.len() <= MAX_DATA_CHUNK_BYTES);

                    match received.last_mut() {
                        Some((last, archive)) if *last == index => archive.extend(chunk),
                        _ => received.push((index, chunk)),
                    }
                }
                SyncFrame::Commit { push_id: committed } => {
                    assert_eq!(committed, push_id);
                    return (push_id, received);
                }
                frame => panic!("unexpected frame in a push: {frame:?}"),
            }
        }
    }
}

/// A connected [`PreviewSyncConnection`] and the operator's end of it.
async fn connection() -> (PreviewSyncConnection<DuplexStream>, Operator) {
    let (cli, operator) = tokio::io::duplex(4 * MAX_DATA_CHUNK_BYTES);
    let (cli, operator) = tokio::join!(
        WebSocketStream::from_raw_socket(cli, Role::Client, None),
        WebSocketStream::from_raw_socket(operator, Role::Server, None),
    );
    (PreviewSyncConnection::new(cli), Operator(operator))
}

/// The CLI reports the sidecar's state until the operator pairs it, then pushes each archive
/// whole, in chunks that the operator can relay, and takes the generation from the sidecar's
/// answer to its push.
#[tokio::test]
async fn push_reaches_the_sidecar_whole_after_pairing() {
    let (mut connection, mut operator) = connection().await;
    let large: Vec<u8> = (0..2 * MAX_DATA_CHUNK_BYTES + 1)
        .map(|byte| byte as u8)
        .collect();
    let archives = vec![(0, large), (2, b"small".to_vec())];

    let cli = async {
        let mut states = Vec::new();
        let paired = connection
            .wait_until_paired(|state| states.push(state))
            .await
            .unwrap();

        let generation = connection.push(5, &archives).await.unwrap();

        (states, paired, generation)
    };

    let sidecar = async {
        operator
            .send(SyncFrame::Status(SidecarState::Disconnected))
            .await;
        operator
            .send(SyncFrame::Status(SidecarState::AwaitingFirstPush))
            .await;
        operator.send(SyncFrame::Paired { generation: None }).await;

        let pushed = operator.recv_push().await;

        // The state can change while the sidecar applies the push.
        operator
            .send(SyncFrame::Status(SidecarState::Synced { generation: 1 }))
            .await;
        operator
            .send(SyncFrame::Applied {
                push_id: pushed.0,
                generation: 1,
            })
            .await;

        pushed
    };

    let ((states, paired, generation), pushed) = tokio::join!(cli, sidecar);

    assert_eq!(
        states,
        [SidecarState::Disconnected, SidecarState::AwaitingFirstPush]
    );
    assert_eq!(paired, None);
    assert_eq!(generation, 1);
    assert_eq!(pushed, (5, archives));
}

/// A push that doesn't land must fail with the reason, so that `mirrord preview up` can tell the
/// user and delete the session, rather than wait or report a generation that the pod doesn't
/// hold.
#[tokio::test]
async fn failed_push_reports_why() {
    async fn push_answered_with(answer: Option<SyncFrame>) -> PreviewSyncError {
        let (mut connection, mut operator) = connection().await;
        let archives = [(0, b"archive".to_vec())];
        let cli = connection.push(5, &archives);

        let sidecar = async {
            operator.recv_push().await;

            match answer {
                Some(answer) => operator.send(answer).await,
                None => operator.0.close(None).await.unwrap(),
            }

            operator
        };

        let (result, _operator) = tokio::join!(cli, sidecar);
        result.unwrap_err()
    }

    let rejected = push_answered_with(Some(SyncFrame::Rejected {
        push_id: 5,
        reason: "`escape/evil.js` leaves the entry".to_owned(),
    }))
    .await;
    assert!(
        matches!(&rejected, PreviewSyncError::Rejected(reason) if reason.contains("leaves the entry")),
        "{rejected}"
    );

    let stopped = push_answered_with(Some(SyncFrame::Error {
        message: "the preview pod's sync sidecar disconnected".to_owned(),
    }))
    .await;
    assert!(
        matches!(&stopped, PreviewSyncError::Operator(message) if message.contains("disconnected")),
        "{stopped}"
    );

    let closed = push_answered_with(None).await;
    assert!(matches!(closed, PreviewSyncError::Closed), "{closed}");

    // An answer to another push, for example one that an earlier CLI sent, is not this push's.
    let other = push_answered_with(Some(SyncFrame::Applied {
        push_id: 4,
        generation: 9,
    }))
    .await;
    assert!(matches!(other, PreviewSyncError::Unexpected(_)), "{other}");
}

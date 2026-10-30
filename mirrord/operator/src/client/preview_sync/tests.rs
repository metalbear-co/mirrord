use std::{collections::BTreeMap, fs, io::Read, path::Path, time::Duration};
#[cfg(unix)]
use std::{
    ffi::OsStr,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
};

use flate2::read::GzDecoder;
use futures::{SinkExt, StreamExt};
use tokio::io::DuplexStream;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{PackedEntry, PreviewSyncConnection, PreviewSyncError, TreeListing};
use crate::sync::{
    EntryCounts, EntryPlan, MAX_DATA_CHUNK_BYTES, PushMode, RejectReason, SYNC_PROTOCOL_VERSION,
    SidecarState, SyncFrame,
    rules::{IgnoreRules, IgnoreSnapshot},
};

/// How long a test waits for a push before it fails, so that a push that stalls fails the test
/// rather than hang it.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// What an archive holds at one path.
#[derive(Debug, PartialEq, Eq)]
enum Packed {
    Directory,

    /// A file with its contents and whether its owner may execute it.
    File(String, bool),

    /// A symlink with its target.
    Symlink(String),
}

/// The entry that a push of `local` carries, with `ignore` as the entry's own gitignore lines.
fn pack(local: &Path, ignore: &[&str]) -> PackedEntry {
    let ignore = ignore
        .iter()
        .map(|line| (*line).to_owned())
        .collect::<Vec<_>>();
    TreeListing::new(local, &ignore)
        .unwrap()
        .pack(0, local)
        .unwrap()
}

/// What the archive of `entry` holds, by path.
fn unpacked(entry: &PackedEntry) -> BTreeMap<String, Packed> {
    let mut archive = tar::Archive::new(GzDecoder::new(entry.archive.as_slice()));
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
    // The push leaves out the directories of version control systems.
    write(".git/HEAD", "ref: refs/heads/main");
    write(".jj/.gitignore", "/*\n");
    write(".jj/repo/store", "store");
    symlink("app.js", local.path().join("link")).unwrap();

    let file = |contents: &str, executable| Packed::File(contents.to_owned(), executable);
    assert_eq!(
        unpacked(&pack(local.path(), &[])),
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

    let entry = pack(&home.path().join("repo/frontend"), &[]);
    assert_eq!(
        unpacked(&entry),
        BTreeMap::from([("app.js".to_owned(), Packed::File("app".to_owned(), false))])
    );

    // The sidecar keeps on the volume what the rules of the push ignore, so they must ignore
    // what the push left out, with the patterns of the repository's root.
    let rules = IgnoreRules::new(entry.plan.rules.clone());
    for (left_out, is_dir) in [
        (".env", false),
        ("notes.txt", false),
        ("node_modules", true),
    ] {
        assert!(rules.is_ignored(Path::new(left_out), is_dir), "{left_out}");
    }
    assert!(!rules.is_ignored(Path::new("app.js"), false));
}

/// A repository can name its own excludes file in its configuration, which takes precedence over
/// the developer's global one, and a push must leave out what that file excludes, which may be a
/// secret, however git's syntax writes the setting.
#[cfg(unix)]
#[test]
// Git is an `.exe` on Windows, which `Command::new` finds.
#[allow(clippy::disallowed_methods)]
fn push_leaves_out_what_the_repository_configuration_excludes() {
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo.path())
            // Keeps the developer's own configuration out of the comparison.
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        output.status.code()
    };

    assert_eq!(git(&["init", "--quiet"]), Some(0));
    let excludes = repo.path().join("private-excludes");
    fs::write(&excludes, "private-key.pem\n").unwrap();
    assert_eq!(
        git(&["config", "core.excludesFile", excludes.to_str().unwrap()]),
        Some(0)
    );
    fs::write(repo.path().join("app.js"), "app").unwrap();
    fs::write(repo.path().join("private-key.pem"), "secret").unwrap();

    assert_eq!(
        git(&["check-ignore", "--no-index", "--quiet", "private-key.pem"]),
        Some(0)
    );
    let paths = unpacked(&pack(repo.path(), &[]))
        .into_keys()
        .collect::<Vec<_>>();
    assert_eq!(paths, ["app.js", "private-excludes"]);

    // A later `[core]` section wins, with an inline comment, with a quoted path followed by a
    // comment, with a path relative to the working tree, and in a file that the configuration
    // includes.
    let config = repo.path().join(".git/config");
    let base = fs::read_to_string(&config).unwrap();
    let included = repo.path().join(".git/included");
    fs::write(
        &included,
        format!("[core]\n\texcludesFile = {}\n", excludes.display()),
    )
    .unwrap();
    for section in [
        format!("[core]\n\texcludesFile = {} # the keys", excludes.display()),
        format!(
            "[core]\n\texcludesFile = \"{}\" ; the keys",
            excludes.display()
        ),
        "[core]\n\texcludesFile = private-excludes".to_owned(),
        format!("[include]\n\tpath = {}", included.display()),
    ] {
        fs::write(&config, format!("{base}{section}\n")).unwrap();

        assert_eq!(
            git(&["check-ignore", "--no-index", "--quiet", "private-key.pem"]),
            Some(0),
            "{section}"
        );
        let paths = unpacked(&pack(repo.path(), &[]))
            .into_keys()
            .collect::<Vec<_>>();
        assert_eq!(paths, ["app.js", "private-excludes"], "{section}");
    }
}

/// A `.gitignore` in a directory whose name isn't UTF-8 applies to that directory's files, as in
/// git.
#[cfg(unix)]
#[test]
fn push_leaves_out_what_a_gitignore_in_a_directory_with_a_non_utf8_name_excludes() {
    let local = tempfile::tempdir().unwrap();
    let directory = local.path().join(OsStr::from_bytes(b"caf\xe9"));
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join(".gitignore"), "private-key.pem\n").unwrap();
    fs::write(directory.join("app.js"), "app").unwrap();
    fs::write(directory.join("private-key.pem"), "secret").unwrap();

    let paths = unpacked(&pack(local.path(), &[]))
        .into_keys()
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert!(
        !paths.iter().any(|path| path.ends_with("private-key.pem")),
        "{paths:?}"
    );
}

/// A file that fails to pack in a large tree must be named, so that the developer can find it.
#[test]
fn a_file_that_fails_to_pack_is_named() {
    let local = tempfile::tempdir().unwrap();
    fs::create_dir(local.path().join("src")).unwrap();
    fs::write(local.path().join("src/app.js"), "app").unwrap();
    let listing = TreeListing::new(local.path(), &[]).unwrap();

    fs::remove_file(local.path().join("src/app.js")).unwrap();
    let error = listing.pack(0, local.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("`{}`", local.path().join("src/app.js").display())),
        "{error}"
    );
}

/// The entry's own `ignore` lines leave out more, and re-include what a `.gitignore` excludes,
/// but never a file below an ignored directory. A `.gitignore` in the tree counts outside a
/// repository too.
#[test]
fn push_leaves_out_what_the_entry_ignores() {
    let local = tempfile::tempdir().unwrap();
    let write = |path: &str| {
        let path = local.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    };

    fs::write(local.path().join(".gitignore"), ".env*\ndist/\n").unwrap();
    write("app.js");
    write(".env");
    write(".env.preview");
    write("coverage/index.html");
    write("dist/.env.preview");

    let paths = unpacked(&pack(
        local.path(),
        &["coverage/", "!.env.preview", "!dist/.env.preview"],
    ))
    .into_keys()
    .collect::<Vec<_>>();
    assert_eq!(paths, [".env.preview", ".gitignore", "app.js"]);
}

/// The sidecar refuses a stream that holds more than `Begin` declared, so the declared counts
/// must be those of the stream.
#[cfg(unix)]
#[test]
fn push_declares_what_its_stream_holds() {
    let local = tempfile::tempdir().unwrap();
    fs::create_dir(local.path().join("lib")).unwrap();
    fs::write(local.path().join("lib/util.js"), "util").unwrap();
    fs::write(local.path().join("app.js"), "app, longer").unwrap();
    symlink("app.js", local.path().join("link")).unwrap();

    let entry = pack(local.path(), &[]);
    let mut archive = tar::Archive::new(GzDecoder::new(entry.archive.as_slice()));
    let (mut files, mut directories, mut bytes) = (0, 0, 0);
    for archived in archive.entries().unwrap() {
        let header = archived.unwrap().header().clone();
        if header.entry_type().is_dir() {
            directories += 1;
        } else {
            files += 1;
            bytes += header.size().unwrap();
        }
    }

    assert_eq!(
        (entry.plan.files, entry.plan.directories, entry.plan.bytes),
        (files, directories, bytes)
    );
    assert_eq!((files, directories, bytes), (3, 1, 15));
}

/// A dev server's socket in the tree must not stop the push of the files around it.
#[cfg(unix)]
#[test]
fn push_skips_sockets() {
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("app.js"), "app").unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(local.path().join("dev.sock")).unwrap();

    assert_eq!(
        unpacked(&pack(local.path(), &[])),
        BTreeMap::from([("app.js".to_owned(), Packed::File("app".to_owned(), false))])
    );
}

/// `--watch` pushes the tree again only when its listing changes, so a build that writes into an
/// ignored directory, even inside a pushed one, must not change it, and an edit of a pushed file
/// must, even when the edit keeps the file's size and modification time.
#[test]
fn listing_changes_only_with_the_pushed_tree() {
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join(".gitignore"), "dist/\n*.log\n").unwrap();
    fs::create_dir(local.path().join("src")).unwrap();
    fs::write(local.path().join("src/app.js"), "v1").unwrap();
    let pushed = TreeListing::new(local.path(), &[]).unwrap();

    fs::create_dir(local.path().join("dist")).unwrap();
    fs::write(local.path().join("dist/bundle.js"), "bundle").unwrap();
    fs::write(local.path().join("src/debug.log"), "log").unwrap();
    assert_eq!(TreeListing::new(local.path(), &[]).unwrap(), pushed);

    fs::write(local.path().join("src/app.js"), "v2, longer").unwrap();
    assert_ne!(TreeListing::new(local.path(), &[]).unwrap(), pushed);
}

/// Tools such as `cp -p` write a file and then restore its modification time, and an edit that
/// keeps the size then looks unchanged by size and time alone.
#[cfg(unix)]
#[test]
fn listing_changes_with_an_edit_that_keeps_size_and_time() {
    let local = tempfile::tempdir().unwrap();
    let app = local.path().join("app.js");
    fs::write(&app, "v1").unwrap();
    let modified = fs::metadata(&app).unwrap().modified().unwrap();
    let pushed = TreeListing::new(local.path(), &[]).unwrap();

    // The status change time has a resolution of a clock tick, so the edit must come later.
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&app, "v2").unwrap();
    fs::File::options()
        .write(true)
        .open(&app)
        .unwrap()
        .set_modified(modified)
        .unwrap();

    assert_ne!(TreeListing::new(local.path(), &[]).unwrap(), pushed);
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

    /// Receives a full push and returns its id and its entries, each with the plan that `Begin`
    /// declared and what its `Data` frames carried, in the order they came. Checks that each
    /// entry's data ends with `EntryDone`.
    async fn recv_push(&mut self) -> (u64, Vec<PackedEntry>) {
        let SyncFrame::Begin {
            push_id,
            mode: PushMode::Full,
            entries: plans,
        } = self.recv().await
        else {
            panic!("a push starts with `Begin` of a full push");
        };

        let mut received = Vec::<PackedEntry>::new();
        let mut open = None;

        loop {
            match self.recv().await {
                SyncFrame::Data { index, chunk } => {
                    assert!(chunk.len() <= MAX_DATA_CHUNK_BYTES);

                    if open != Some(index) {
                        assert_eq!(open, None, "entry {index} began before the last one ended");
                        let plan = plans
                            .iter()
                            .find(|plan| plan.index == index)
                            .unwrap_or_else(|| panic!("entry {index} isn't in the push"));
                        received.push(PackedEntry {
                            plan: plan.clone(),
                            archive: Vec::new(),
                        });
                        open = Some(index);
                    }

                    received.last_mut().unwrap().archive.extend(chunk);
                }
                SyncFrame::EntryDone { index, deleted } => {
                    assert_eq!(open.take(), Some(index));
                    assert!(deleted.is_empty(), "a full push lists no deleted paths");
                }
                SyncFrame::Commit { push_id: committed } => {
                    assert_eq!((committed, open), (push_id, None));
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
    let entry = |index, archive| PackedEntry {
        plan: EntryPlan {
            index,
            files: 1,
            directories: 0,
            bytes: 10,
            rules: IgnoreSnapshot::default(),
        },
        archive,
    };
    let large: Vec<u8> = (0..2 * MAX_DATA_CHUNK_BYTES + 1)
        .map(|byte| byte as u8)
        .collect();
    let entries = vec![entry(0, large), entry(2, b"small".to_vec())];

    let cli = async {
        let mut states = Vec::new();
        let paired = connection
            .wait_until_paired(|state| states.push(state))
            .await
            .unwrap();

        let generation = connection.push(5, &entries).await.unwrap();

        (states, paired, generation)
    };

    let sidecar = async {
        operator
            .send(SyncFrame::Status(SidecarState::Disconnected))
            .await;
        operator
            .send(SyncFrame::Status(SidecarState::AwaitingFirstPush))
            .await;
        operator
            .send(SyncFrame::Paired {
                generation: None,
                protocol_version: SYNC_PROTOCOL_VERSION,
            })
            .await;

        let pushed = operator.recv_push().await;

        // The state can change while the sidecar applies the push.
        operator
            .send(SyncFrame::Status(SidecarState::Synced { generation: 1 }))
            .await;
        operator
            .send(SyncFrame::Applied {
                push_id: pushed.0,
                generation: 1,
                entries: Vec::new(),
            })
            .await;

        pushed
    };

    let ((states, paired, generation), pushed) =
        tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(cli, sidecar) })
            .await
            .expect("the push didn't finish");

    assert_eq!(
        states,
        [SidecarState::Disconnected, SidecarState::AwaitingFirstPush]
    );
    assert_eq!(paired, None);
    assert_eq!(generation, 1);
    assert_eq!(pushed, (5, entries));
}

/// A push that doesn't land must fail with the reason, so that `mirrord preview sync` can tell the
/// user and delete the session, rather than wait or report a generation that the pod doesn't
/// hold.
#[tokio::test]
async fn failed_push_reports_why() {
    async fn push_answered_with(answer: Option<SyncFrame>) -> PreviewSyncError {
        let (mut connection, mut operator) = connection().await;
        let entries = [PackedEntry {
            plan: EntryPlan {
                index: 0,
                files: 1,
                directories: 0,
                bytes: 7,
                rules: IgnoreSnapshot::default(),
            },
            archive: b"archive".to_vec(),
        }];
        let cli = connection.push(5, &entries);

        let sidecar = async {
            operator.recv_push().await;

            match answer {
                Some(answer) => operator.send(answer).await,
                None => operator.0.close(None).await.unwrap(),
            }

            operator
        };

        let (result, _operator) =
            tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(cli, sidecar) })
                .await
                .expect("the push didn't finish");
        result.unwrap_err()
    }

    let rejected = push_answered_with(Some(SyncFrame::Rejected {
        push_id: 5,
        reason: RejectReason::Invalid("`../evil.js` leaves the entry".to_owned()),
    }))
    .await;
    assert!(
        matches!(&rejected, PreviewSyncError::Rejected(RejectReason::Invalid(reason)) if reason.contains("leaves the entry")),
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
        entries: vec![EntryCounts {
            index: 0,
            written: 1,
            unchanged: 0,
            deleted: 0,
        }],
    }))
    .await;
    assert!(matches!(other, PreviewSyncError::Unexpected(_)), "{other}");
}

/// jj honours the `.gitignore` files of its repository also when it keeps its git data outside
/// the working tree, so a `local` in such a repository gets the `.gitignore` of the directories
/// above it, as in a git repository.
#[test]
fn a_jj_repository_without_git_applies_the_gitignore_files_above_local() {
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir(repo.path().join(".jj")).unwrap();
    fs::write(repo.path().join(".gitignore"), "*.log\n").unwrap();
    let local = repo.path().join("service");
    fs::create_dir(&local).unwrap();
    fs::write(local.join("app.js"), "app").unwrap();
    fs::write(local.join("debug.log"), "log").unwrap();

    let paths = unpacked(&pack(&local, &[])).into_keys().collect::<Vec<_>>();
    assert_eq!(paths, ["app.js"]);
}
